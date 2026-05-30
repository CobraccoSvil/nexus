"""understanding_node (Cluster 2): comprensione del problema PRIMA di pianificare.

Nuovo nodo del grafo LangGraph inserito tra clarify_or_expand e il routing
planner/executor. Si attiva SOLO per task complessi (gated da complessita' +
token budget). Quando attivo:
  1. Grounding semantico: cerca nel codebase/KB/chat via il tool MCP esistente
     `nexus_search_semantic` (source_kinds code+kb+chat_history).
  2. Fan-out opzionale: spawna sub-agent `explore` in parallelo via il tool MCP
     `dispatch_subagent` (canale esistente).
  3. Produce un `context_brief` (concatenazione strutturata o sintesi LLM
     economica) che il planner inietta nel proprio system prompt.

Flag OFF o task non complesso => pass-through (ritorna {}), path identico a oggi.
Best-effort: ogni errore degrada a no-op, non blocca mai il run.

Integrazione (regola del piano): usa SOLO servizi Nexus esistenti
(nexus_search_semantic, dispatch_subagent via tool_runner; purpose_model per il
modello di sintesi). Niente client embedding/Qdrant nuovi.
"""
from __future__ import annotations

import asyncio
import json
import logging
import uuid
from typing import Any

from . import orchestrator_config
from .state import AgentState

logger = logging.getLogger(__name__)

# Servizi iniettati da configure() (vedi graph.create_agent_graph).
_providers: Any = None
_tool_runner: Any = None
_routing_client: Any = None


def configure(providers: Any = None, tool_runner: Any = None, routing_client: Any = None) -> None:
    global _providers, _tool_runner, _routing_client
    _providers = providers
    _tool_runner = tool_runner
    _routing_client = routing_client


def _is_complex(state: AgentState) -> bool:
    """Riusa i segnali del classifier adattivo (PR-D) per stimare la complessita'."""
    if str(state.get("task_complexity") or "").lower() == "high":
        return True
    if state.get("is_ambiguous"):
        return True
    try:
        ag = state.get("agentic_score")
        if ag is not None and float(ag) >= 0.7:
            return True
    except (TypeError, ValueError):
        pass
    return False


def _last_user_message(state: AgentState) -> str:
    for m in reversed(state.get("messages", []) or []):
        content = getattr(m, "content", None)
        if isinstance(content, str) and content.strip():
            return content.strip()
    return ""


async def understanding_node(state: AgentState) -> dict[str, Any]:
    """Nodo di comprensione pre-planning (Cluster 2). Pass-through se non attivo."""
    cfg = orchestrator_config.get()
    if not bool(cfg.get("understanding_enabled")):
        return {}
    # Depth guard (anti-esplosione esponenziale): l'understanding e' progettato
    # per il main run (subagent_depth=0). Se siamo dentro un sub-agent ed e'
    # attivo il fan-out, ogni sub-agent lancerebbe altri 3 sub-agent explore
    # che a loro volta rilancerebbero... -> esplosione combinatoria osservata
    # in produzione. I sub-agent hanno gia' un task focalizzato, non hanno
    # bisogno di understanding pre-planning.
    if int(state.get("subagent_depth") or 0) >= 1:
        return {"understanding_active": False, "understanding_skip_reason": "skip_in_subagent"}
    # Gate hard: token budget minimo + complessita'.
    token_budget = int(state.get("token_budget") or 0)
    if token_budget < int(cfg.get("understanding_min_token_budget", 3000)):
        return {"understanding_active": False, "understanding_skip_reason": "budget_too_low"}
    if not _is_complex(state):
        return {"understanding_active": False, "understanding_skip_reason": "not_complex"}
    if _tool_runner is None:
        return {"understanding_active": False, "understanding_skip_reason": "tool_runner_missing"}

    query = _last_user_message(state)
    if len(query) < 10:
        return {"understanding_active": False, "understanding_skip_reason": "query_too_short"}

    session_id = str(state.get("session_id") or "")
    topk = int(cfg.get("understanding_topk", 8))

    # ── 1. Grounding semantico (cuore RAG) via nexus_search_semantic ─────────
    grounding_block = ""
    try:
        res = await _tool_runner.execute_tool(
            tool_name="nexus_search_semantic",
            tool_input={
                "query": query,
                "source_kinds": ["code", "kb", "chat_history"],
                "top_k": topk,
            },
            session_id=session_id,
            tool_use_id=str(uuid.uuid4()),
        )
        raw = getattr(res, "result_json", None) or "{}"
        hits = (json.loads(raw).get("hits") or [])[:topk]
        if hits:
            lines = []
            for h in hits:
                sk = h.get("source_kind", "?")
                txt = str(h.get("chunk_text") or "").strip()[:300]
                score = float(h.get("score") or 0)
                if txt:
                    lines.append(f'  <hit source="{sk}" score="{score:.2f}">{txt}</hit>')
            if lines:
                grounding_block = "<grounding>\n" + "\n".join(lines) + "\n</grounding>"
    except Exception as exc:
        logger.debug("understanding_node: grounding semantico fallito (%s)", exc)

    # ── 2. Fan-out explore opzionale via dispatch_subagent (tool MCP) ────────
    explore_block = ""
    if (
        bool(cfg.get("understanding_fanout_enabled"))
        and bool(cfg.get("subagents_enabled", True))
    ):
        max_explore = int(cfg.get("understanding_max_explore", 3))
        # Sotto-domande semplici derivate dal task (euristica leggera, no LLM):
        # esploriamo "come funziona", "dove si trova", "test esistenti".
        subqueries = [
            f"Come funziona e dove si trova nel codebase: {query}",
            f"Test, vincoli e casi limite rilevanti per: {query}",
            f"Dipendenze e impatti di: {query}",
        ][:max_explore]
        try:
            tasks = [
                _tool_runner.execute_tool(
                    tool_name="dispatch_subagent",
                    tool_input={"kind": "explore", "task": sq},
                    session_id=session_id,
                    tool_use_id=str(uuid.uuid4()),
                )
                for sq in subqueries
            ]
            results = await asyncio.gather(*tasks, return_exceptions=True)
            summaries = []
            for r in results:
                if isinstance(r, Exception):
                    continue
                raw = getattr(r, "result_json", None) or "{}"
                try:
                    summary = json.loads(raw).get("summary") or ""
                except Exception:
                    summary = ""
                if summary:
                    summaries.append(f"  <explore>{str(summary)[:400]}</explore>")
            if summaries:
                explore_block = "<esplorazioni>\n" + "\n".join(summaries) + "\n</esplorazioni>"
        except Exception as exc:
            logger.debug("understanding_node: fan-out explore fallito (%s)", exc)

    if not grounding_block and not explore_block:
        return {"understanding_active": False, "understanding_skip_reason": "no_context_found"}

    raw_brief = "\n\n".join(b for b in (grounding_block, explore_block) if b)

    # ── 3. Sintesi opzionale (LLM economico) o concatenazione strutturata ────
    context_brief = raw_brief
    if bool(cfg.get("understanding_synthesize_enabled")) and _providers is not None and _routing_client is not None:
        try:
            decision = _routing_client.purpose_model(purpose="understanding")
            provider, model = decision.provider, decision.model
            if provider and not provider.startswith("__"):
                prompt = (
                    "Sintetizza il contesto seguente in un brief conciso (max 200 parole) "
                    "utile a un planner per affrontare il task. Elenca: cosa esiste gia', "
                    "vincoli noti, file/punti rilevanti.\n\n"
                    f"Task: {query}\n\n{raw_brief}"
                )
                result = await asyncio.to_thread(
                    _providers.generate_completion, provider, model, prompt
                )
                synth = (getattr(result, "content", "") or "").strip()
                if synth:
                    context_brief = "<context_brief>\n" + synth + "\n</context_brief>"
        except Exception as exc:
            logger.debug("understanding_node: sintesi fallita (%s), uso brief grezzo", exc)

    logger.info(
        "understanding_node: context_brief prodotto (%d char, grounding=%s, explore=%s)",
        len(context_brief), bool(grounding_block), bool(explore_block),
    )
    return {
        "understanding_active": True,
        "context_brief": context_brief,
    }
