"""subagent_dispatch_node (PR-3): spawn di un sub-agent runtime con context isolato.

Diversamente da planner_node + verifier_node, questo nodo NON e' parte
del grafo LangGraph principale: viene chiamato direttamente come ENTRY
POINT dell'endpoint REST `POST /agent/subagent-run` esposto dal brain.

Flusso:
  1. Carica nexus_subagent_definitions per il kind richiesto
  2. Carica system_text dal prompt_registry (definition.prompt_key)
  3. Costruisce uno state iniziale FRESCO (no history del main):
      - messages = [HumanMessage(task + context)]
      - profile_name = None (skipping profile router)
      - system_text = prompt_key del kind
      - tools_json = filtrato sulla tool_whitelist della definition
      - thread_id = subagent_run_id (cosi' checkpointer lo isola)
  4. Invoca lo stesso agent_graph.ainvoke() esistente
  5. Aspetta il termine (timeout dalla definition)
  6. Estrae final_answer + token usage + costo + artifacts

Niente sub-graph custom: riusa l'agent loop completo (router → executor →
tool_dispatch → reflection → learner). Il sub-agent eredita cascade M60,
M58 port allocation, ecc.
"""
from __future__ import annotations

import asyncio
import logging
import os
import re
import time
from typing import Any

from langchain_core.messages import HumanMessage

from . import prompt_registry, subagent_store

logger = logging.getLogger(__name__)

# Stesso endpoint interno no-auth gia' usato da planner_node._retrieve_decision_context:
# riuso del servizio di ricerca vettoriale LOCALE (Qdrant+Postgres), niente
# client nuovo, niente egress verso provider (vincolo riservatezza dati).
_MCP_CORE_INTERNAL_URL = os.environ.get("MCP_CORE_INTERNAL_URL", "http://localhost:4000")


async def run_subagent(
    *,
    subagent_run_id: str,
    parent_run_id: str,
    project_id: str,
    user_id: str,
    session_id: str,
    kind: str,
    task: str,
    context: str = "",
    expected_format: str = "",
    depth: int = 1,
    is_background: bool = False,
    agent_graph: Any,
) -> dict[str, Any]:
    """Spawn di una sub-run isolata. Ritorna summary compatto.

    Args:
        subagent_run_id: UUID gia' inserito in nexus_subagent_runs (status=pending)
        parent_run_id: UUID dell'agent_run padre
        project_id, user_id, session_id: ereditati dal context del main
        kind: nome del sub-agent kind (deve esistere in nexus_subagent_definitions)
        task: descrizione del task (auto-contained)
        context: contesto opzionale (file rilevanti, vincoli)
        expected_format: forma del summary atteso
        depth: profondita corrente (1 = chiamato dal main)
        is_background: se True, ritorna subito con status='running'
                       (NOT IMPLEMENTED in PR-3 lean: sempre sincrono)
        agent_graph: handle del grafo LangGraph compilato

    Returns:
        dict con {summary, status, iterations, cost_usd, tokens, artifacts}
    """
    started = time.monotonic()

    # 1. Carica definition
    definition = subagent_store.fetch_definition(kind)
    if not definition:
        summary = f"[kind '{kind}' non trovato in nexus_subagent_definitions]"
        subagent_store.update_run_completion(
            subagent_run_id, status="failed", final_summary=summary,
            artifacts=[], iterations=0,
            tokens_prompt=0, tokens_completion=0, cost_usd=0.0,
        )
        return {
            "status": "failed", "summary": summary,
            "iterations": 0, "cost_usd": 0, "tokens": {}, "artifacts": [],
        }

    # 2. Carica prompt
    prompt_key = definition["prompt_key"]
    system_text = prompt_registry.get_prompt(prompt_key) or ""
    if not system_text:
        summary = f"[prompt '{prompt_key}' non trovato]"
        subagent_store.update_run_completion(
            subagent_run_id, status="failed", final_summary=summary,
            artifacts=[], iterations=0,
            tokens_prompt=0, tokens_completion=0, cost_usd=0.0,
        )
        return {
            "status": "failed", "summary": summary,
            "iterations": 0, "cost_usd": 0, "tokens": {}, "artifacts": [],
        }

    # 2b. Componente B: contesto continuo via memoria vettoriale (gated).
    # Arricchisce il system_text con (a) grounding RAG locale sul task,
    # (b) rationale del parent (da nexus_agent_plans). NIENTE dump del contesto
    # del parent: solo snippet locali + rationale strutturato (riservatezza).
    try:
        from . import orchestrator_config
        _ocfg = orchestrator_config.get()
        if _ocfg.get("subagent_rag_grounding_enabled"):
            grounding = _ground_subagent_context(project_id, task, _ocfg)
            if grounding:
                system_text = system_text + "\n\n" + grounding
        if _ocfg.get("subagent_inherit_plan_rationale"):
            parent_rat = _fetch_parent_rationale(parent_run_id)
            if parent_rat:
                system_text = system_text + "\n\n" + parent_rat
    except Exception as exc:
        logger.debug("run_subagent: arricchimento contesto (B) skip: %s", exc)

    # 3. Build messages iniziali. Embed il context come parte del task per il sub-agent.
    initial_text = task.strip()
    if context.strip():
        initial_text += f"\n\n## Contesto aggiuntivo\n{context.strip()}"
    if expected_format.strip():
        initial_text += f"\n\n## Formato output atteso\n{expected_format.strip()}"

    # 4. Costruisci tool catalog filtrato per la whitelist
    tool_whitelist = list(definition.get("tool_whitelist") or [])
    tools_json = _filter_tools_by_whitelist(tool_whitelist)

    # 4b. Risolvi il modello del worker dal model_purpose della definition.
    # Senza questo, il worker risolverebbe un intent inesistente
    # ("subagent_<kind>") cadendo su un fallback generico: i worker NON
    # userebbero i modelli economici/specifici previsti. Iniettiamo
    # provider_override/model_override (l'executor li rispetta per primi,
    # vedi nodes.py executor_node). Tutto DB-driven via nexus_purpose_model.
    worker_provider: str | None = None
    worker_model: str | None = None
    model_purpose = (definition.get("model_purpose") or "").strip()
    if model_purpose:
        try:
            from brain.router.service import _routing_client_singleton
            decision = _routing_client_singleton().purpose_model(purpose=model_purpose)
            # __router_unavailable__ / payload malformato: NON forziamo
            # override, lasciamo che l'executor risolva via routing normale.
            if decision.provider and not decision.provider.startswith("__"):
                worker_provider = decision.provider
                worker_model = decision.model
                logger.info(
                    "run_subagent: kind=%s model_purpose=%s -> %s/%s",
                    kind, model_purpose, worker_provider, worker_model,
                )
            else:
                logger.warning(
                    "run_subagent: kind=%s model_purpose=%s non risolto (%s) — "
                    "executor usera' il routing di default",
                    kind, model_purpose, decision.rationale,
                )
        except Exception as exc:
            logger.warning(
                "run_subagent: risoluzione model_purpose=%s fallita (%s) — "
                "executor usera' il routing di default", model_purpose, exc,
            )

    # 5. Stato iniziale fresco (no history del main)
    initial_state: dict[str, Any] = {
        "messages": [HumanMessage(content=initial_text)],
        "thread_id": subagent_run_id,
        "session_id": session_id,
        "user_intent": "subagent_" + kind,
        "task_type": "subagent_" + kind,
        "behavior_mode": "automatico",
        "token_budget": 4096,
        "system_text": system_text,
        "tools_json": tools_json,
        "profile_name": None,  # skip profilo router
        "parent_run_id": parent_run_id,
        "subagent_depth": int(depth),
        "iterations": 0,
        "approved": True,  # auto-approvato (sub-agent)
        "plan_phase_active": False,  # sub-agent non rifa il planning
    }
    # Inietta gli override solo se risolti (altrimenti routing di default).
    if worker_provider and worker_model:
        initial_state["provider_override"] = worker_provider
        initial_state["model_override"] = worker_model

    # 6. Marca running
    subagent_store.update_run_start(subagent_run_id)

    # 7. Invoca grafo con timeout
    timeout_s = float(definition.get("timeout_s") or 300)
    try:
        config = {"configurable": {"thread_id": subagent_run_id}}
        result_state = await asyncio.wait_for(
            agent_graph.ainvoke(initial_state, config=config),
            timeout=timeout_s,
        )
    except asyncio.TimeoutError:
        subagent_store.update_run_completion(
            subagent_run_id, status="timeout",
            final_summary="[Sub-agent timeout]",
            artifacts=[], iterations=0,
            tokens_prompt=0, tokens_completion=0, cost_usd=0.0,
        )
        return {
            "status": "timeout",
            "summary": "[Sub-agent timeout]",
            "iterations": 0, "cost_usd": 0, "tokens": {},
            "artifacts": [],
        }
    except Exception as exc:
        logger.error("subagent_dispatch_node: agent_graph fallito: %s", exc)
        subagent_store.update_run_completion(
            subagent_run_id, status="failed",
            final_summary=f"[errore: {exc}]",
            artifacts=[], iterations=0,
            tokens_prompt=0, tokens_completion=0, cost_usd=0.0,
        )
        return {
            "status": "failed",
            "summary": f"[errore grafo: {exc}]",
            "iterations": 0, "cost_usd": 0, "tokens": {},
            "artifacts": [],
        }

    # 8. Estrai summary + metriche
    final_text = _extract_final_text(result_state)
    iterations = int(result_state.get("iterations") or 0)
    prompt_tokens = int(result_state.get("prompt_tokens") or 0)
    completion_tokens = int(result_state.get("completion_tokens") or 0)
    cost_usd = float(result_state.get("total_cost_usd") or 0.0)
    artifacts = _extract_artifacts(result_state)

    # 9. Persisti e ritorna
    subagent_store.update_run_completion(
        subagent_run_id, status="completed",
        final_summary=final_text[:4000],
        artifacts=artifacts,
        iterations=iterations,
        tokens_prompt=prompt_tokens,
        tokens_completion=completion_tokens,
        cost_usd=cost_usd,
    )

    duration_ms = int((time.monotonic() - started) * 1000)
    logger.info(
        "subagent_dispatch_node: kind=%s run_id=%s iter=%d cost=$%.4f duration=%dms",
        kind, subagent_run_id, iterations, cost_usd, duration_ms,
    )

    return {
        "subagent_run_id": subagent_run_id,
        "kind": kind,
        "status": "completed",
        "summary": _compact_summary(final_text),
        "artifacts": artifacts,
        "iterations": iterations,
        "cost_usd": round(cost_usd, 6),
        "tokens": {"prompt": prompt_tokens, "completion": completion_tokens},
    }


def _ground_subagent_context(project_id: str, task: str, cfg: dict) -> str:
    """Componente B: grounding RAG locale sul task del sub-agent.

    Riusa /api/internal/knowledge/search (Qdrant+Postgres locali). Best-effort,
    mai solleva. Ritorna un blocco <memoria_progetto> o "".
    """
    pid = str(project_id or "").strip()
    if not pid or len(task.strip()) < 10:
        return ""
    try:
        import requests  # noqa: PLC0415
        resp = requests.post(
            f"{_MCP_CORE_INTERNAL_URL}/api/internal/knowledge/search",
            json={
                "project_id": pid,
                "query": task.strip(),
                "top_k": int(cfg.get("subagent_rag_grounding_topk", 5)),
                "min_score": float(cfg.get("subagent_rag_grounding_min_score", 0.55)),
            },
            timeout=5,
        )
        if resp.status_code != 200:
            return ""
        results = resp.json().get("results", []) or []
    except Exception as exc:
        logger.debug("run_subagent: grounding RAG fallito: %s", exc)
        return ""
    if not results:
        return ""
    snippet_max = int(cfg.get("subagent_rag_grounding_snippet_max", 800))
    lines = []
    for r in results:
        title = str(r.get("title") or "").strip()
        snippet = str(r.get("snippet") or "").strip()
        if len(snippet) > snippet_max:
            snippet = snippet[: snippet_max - 3] + "..."
        score = float(r.get("score") or 0)
        if title or snippet:
            lines.append(f'  <nota score="{score:.2f}"><titolo>{title}</titolo><contenuto>{snippet}</contenuto></nota>')
    if not lines:
        return ""
    logger.info("run_subagent: grounding RAG iniettato (%d note)", len(lines))
    return (
        "<memoria_progetto>\n"
        "  <!-- Contesto rilevante dalla memoria vettoriale del progetto. -->\n"
        + "\n".join(lines)
        + "\n</memoria_progetto>"
    )


def _fetch_parent_rationale(parent_run_id: str) -> str:
    """Componente B: recupera il rationale del piano del parent (mig 0206).

    SELECT su nexus_agent_plans, riusa subagent_store._conn(). Solo rationale
    STRUTTURATO, nessun dump della conversazione del parent (riservatezza).
    """
    if not parent_run_id:
        return ""
    conn = subagent_store._conn()
    if conn is None:
        return ""
    try:
        with conn.cursor() as cur:
            cur.execute(
                "SELECT rationale, constraints FROM nexus_agent_plans WHERE run_id = %s",
                (parent_run_id,),
            )
            row = cur.fetchone()
    except Exception as exc:
        logger.debug("run_subagent: fetch rationale parent fallito: %s", exc)
        return ""
    finally:
        conn.close()
    if not row:
        return ""
    rationale = (row.get("rationale") if isinstance(row, dict) else row[0]) or ""
    rationale = str(rationale).strip()
    if not rationale:
        return ""
    return (
        "<rationale_parent>\n"
        "  <!-- Razionale del piano del parent: rispetta queste scelte. -->\n  "
        + rationale[:1500]
        + "\n</rationale_parent>"
    )


def _filter_tools_by_whitelist(whitelist: list[str]) -> list[dict]:
    """Costruisce un tool catalog minimale dalla whitelist.

    NOTA: Per evitare di duplicare l'intero TOOL_CATALOG Rust qui in Python,
    passiamo una lista di descrittori SOLO con name+description+input_schema
    minimo. Il vero validation avviene server-side al momento del dispatch.
    Per PR-3 lean, ritorniamo schema generici (the agent inferira' i parametri
    dai tipi di tool comuni).
    """
    if not whitelist:
        return []
    # Schemi compatti per i tool comuni. Il modello sa gia' come usarli.
    base_descriptions = {
        "list_files": {"description": "Lista i file di una directory del progetto."},
        "read_file": {"description": "Legge il contenuto di un file."},
        "write_file": {"description": "Scrive un file nel progetto."},
        "edit_file": {"description": "Modifica parziale di un file (find/replace mirato)."},
        "search_in_files": {"description": "Ricerca testuale ricorsiva nei file del progetto."},
        "run_command": {"description": "Esegue un comando shell nel workspace del progetto."},
        "recall_context": {"description": "Recupera context semantico dalla memoria del progetto."},
        "search_codebase_semantic": {"description": "Ricerca semantica sul codebase indicizzato."},
        "nexus_todo_write": {"description": "Crea/aggiorna TODO list strutturata."},
    }
    out = []
    for tool_name in whitelist:
        desc = base_descriptions.get(tool_name, {"description": "Tool standard Nexus."})
        out.append({
            "name": tool_name,
            "description": desc["description"],
            "input_schema": {"type": "object", "properties": {}, "additionalProperties": True},
        })
    return out


def _extract_final_text(state: dict[str, Any]) -> str:
    """Cerca l'ultimo AIMessage senza tool_use per recuperare il final_answer."""
    messages = state.get("messages") or []
    for m in reversed(messages):
        if not hasattr(m, "content"):
            continue
        # Filtra fuori tool_result blocks
        content = m.content if isinstance(m.content, str) else str(m.content)
        if content and not content.startswith("[Errore"):
            return content
    return state.get("result") or "(no final answer)"


def _extract_artifacts(state: dict[str, Any]) -> list[str]:
    """Best-effort: trova path di file modificati nei tool_result della run."""
    artifacts: list[str] = []
    messages = state.get("messages") or []
    pattern = re.compile(r"File [\"']?([\w./-]+)[\"']?\s*(?:scritto|modificato|creato)")
    for m in messages:
        kwargs = getattr(m, "additional_kwargs", None) or {}
        blocks = kwargs.get("anthropic_content") or []
        for b in blocks:
            if isinstance(b, dict) and b.get("type") == "tool_result":
                txt = str(b.get("content") or "")
                for match in pattern.finditer(txt):
                    p = match.group(1)
                    if p not in artifacts:
                        artifacts.append(p)
    return artifacts[:20]


_TRUNC_SUFFIX = "...[truncated]"


def _compact_summary(text: str, max_chars: int = 600) -> str:
    """Tronca il summary al main: il main riceve solo questo, non l'intera conv."""
    if len(text) <= max_chars:
        return text
    return text[: max_chars - len(_TRUNC_SUFFIX)] + _TRUNC_SUFFIX
