"""Endpoint agente LangGraph e correlati: project-analyze, sub-agent,
clarifying questions HITL, batch-analyze (Anthropic), e gli endpoint del grafo
(run/approve/state/feedback/stats) inclusi gli streaming SSE.

Lo stato condiviso (grafo, provider registry, reload chiavi) vive in
`brain.grpc_server.runtime` e viene letto via attributo di modulo.
"""
from __future__ import annotations

import asyncio
import json as json_mod
import logging
import os
import time

from fastapi import APIRouter
from fastapi.responses import StreamingResponse
from pydantic import BaseModel

from brain.grpc_server import runtime
from brain.utils.db_pool import get_db_url

logger = logging.getLogger(__name__)

router = APIRouter()


def _build_history_messages(conversation_history: list[dict] | None) -> list:
    """Costruisce la lista di AIMessage/HumanMessage di LangChain a partire da
    `body.conversation_history`. Punto unico (regola L / ADR 0026, S72) per il
    pattern duplicato negli endpoint `/agent/run` e `/agent/run-stream`.
    """
    from langchain_core.messages import AIMessage as _AIMessage
    from langchain_core.messages import HumanMessage as _HumanMessage

    history_msgs: list = []
    for msg in (conversation_history or []):
        role = msg.get("role", "user")
        content = msg.get("content", "")
        if not content:
            continue
        # Difesa in profondita' (regola L): i ruoli interni Nexus non-LLM (es.
        # 'summary' iniettato dal compact) NON devono arrivare a un provider che
        # accetta solo user/assistant/system/tool, altrimenti si ha l'errore
        # "unknown variant `summary`" / "[Error: Unexpected role]". mcp-core li
        # normalizza gia' (db_role_to_llm_role), ma qui ribadiamo: solo
        # 'assistant' -> AIMessage, qualunque altro ruolo -> HumanMessage.
        if role == "assistant":
            history_msgs.append(_AIMessage(content=content))
        else:
            if role not in ("user", "assistant", "system"):
                logger.warning(
                    "conversation_history: ruolo non-standard '%s' normalizzato a "
                    "'user' (path a monte non normalizzato?)",
                    role,
                )
            history_msgs.append(_HumanMessage(content=content))
    return history_msgs


# ── PR-3 sub-agents: endpoint per dispatch_subagent (chiamato da mcp-core) ──

class SubagentRunRequest(BaseModel):
    subagent_run_id: str
    parent_run_id: str
    project_id: str
    user_id: str
    session_id: str
    kind: str
    task: str
    context: str = ""
    expected_format: str = ""
    depth: int = 1
    is_background: bool = False


@router.post("/agent/subagent-run")
async def subagent_run_endpoint(body: SubagentRunRequest) -> dict[str, object]:
    """PR-3: spawn di un sub-agent isolato.

    Chiamato dal handler Rust tool_dispatch_subagent dopo aver inserito
    la row in nexus_subagent_runs con status='pending'.
    Riusa l'agent_graph esistente con state fresco e thread_id figlio.
    """
    from brain.agents import subagent_dispatch_node
    graph = runtime._get_agent_graph()
    result = await subagent_dispatch_node.run_subagent(
        subagent_run_id=body.subagent_run_id,
        parent_run_id=body.parent_run_id,
        project_id=body.project_id,
        user_id=body.user_id,
        session_id=body.session_id,
        kind=body.kind,
        task=body.task,
        context=body.context,
        expected_format=body.expected_format,
        depth=body.depth,
        is_background=body.is_background,
        agent_graph=graph,
    )
    return result


# ── PR-3 sub-agent control endpoints ─────────────────────────────────────────
@router.get("/agent/subagent-run/{run_id}")
async def subagent_poll_endpoint(run_id: str) -> dict[str, object]:
    """Poll dello stato di una sub-run. Usato dal main agent quando il
    sub-agent gira in background (is_background=true): il tool dispatch
    ritorna subito con status=running, il main poi fa polling.
    """
    from brain.agents import subagent_store
    row = subagent_store.fetch_run(run_id)
    if not row:
        return {"error": "not_found", "run_id": run_id}
    return {
        "subagent_run_id": row["id"],
        "status": row["status"],
        "kind": row["kind"],
        "summary": row.get("final_summary"),
        "artifacts": row.get("artifacts") or [],
        "iterations": row.get("iterations") or 0,
        "tokens": {
            "prompt": row.get("tokens_prompt") or 0,
            "completion": row.get("tokens_completion") or 0,
        },
        "cost_usd": float(row.get("cost_usd") or 0.0),
        "depth": row.get("depth") or 1,
        "source": row.get("source") or "db",
    }


class SubagentResumeRequest(BaseModel):
    run_id: str


@router.post("/agent/subagent-resume")
async def subagent_resume_endpoint(body: SubagentResumeRequest) -> dict[str, object]:
    """Riprende una sub-run paused/background. Marca lo status come running
    e ritorna subito; il sub-agent viene rilanciato in background dal node.
    """
    from brain.agents import subagent_store
    row = subagent_store.fetch_run(body.run_id)
    if not row:
        return {"status": "error", "error": "not_found", "run_id": body.run_id}
    if row["status"] not in ("paused", "running"):
        return {"status": "noop", "run_id": body.run_id, "current_status": row["status"]}
    subagent_store.update_run_start(body.run_id)
    return {"status": "running", "run_id": body.run_id}


# ── PR-3 clarifying questions HITL ───────────────────────────────────────────
@router.get("/agent/clarifications/{run_id}")
async def clarifications_get(run_id: str) -> dict[str, object]:
    """Ritorna le clarifying questions emesse per un run + eventuali risposte."""
    import os
    url = os.environ.get("DATABASE_URL")
    if not url:
        return {"error": "db_unavailable"}
    try:
        import psycopg2  # type: ignore[import-untyped]
        from psycopg2.extras import RealDictCursor  # type: ignore[import-untyped]
        with psycopg2.connect(url, cursor_factory=RealDictCursor) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    """SELECT id::text, run_id::text, questions, user_answers, applied_defaults,
                              created_at, answered_at
                       FROM nexus_agent_clarifications WHERE run_id = %s
                       ORDER BY created_at DESC LIMIT 1""",
                    (run_id,),
                )
                row = cur.fetchone()
                if not row:
                    return {"run_id": run_id, "clarification": None}
                return {"run_id": run_id, "clarification": dict(row)}
    except Exception as exc:
        return {"error": str(exc)}


class ClarificationsAnswerRequest(BaseModel):
    answers: dict[str, str]


@router.post("/agent/clarifications/{run_id}/answer")
async def clarifications_answer(run_id: str, body: ClarificationsAnswerRequest) -> dict[str, object]:
    """Riceve le risposte dell'utente alle clarifying questions (HITL Confirm).
    Il loop dell'agente puo' poi riprendere il planner con queste risposte
    iniettate come default applicati.
    """
    import os, json as _json
    url = os.environ.get("DATABASE_URL")
    if not url:
        return {"error": "db_unavailable"}
    try:
        import psycopg2  # type: ignore[import-untyped]
        with psycopg2.connect(url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    """UPDATE nexus_agent_clarifications
                       SET user_answers = %s::jsonb, answered_at = NOW()
                       WHERE run_id = %s AND user_answers IS NULL""",
                    (_json.dumps(body.answers), run_id),
                )
            conn.commit()
        return {"status": "ok", "run_id": run_id, "applied": len(body.answers)}
    except Exception as exc:
        return {"error": str(exc)}


# ── Project Analyzer Agent ──────────────────────────────────────────────────
# Endpoint dedicato all'agente agent.project.analyzer (vedi migrazione 0094):
# carica il prompt dal DB, sostituisce i placeholder col payload del progetto,
# chiama il provider con fallback chain, parsa il JSON risultante.
# Il chiamante e' l'endpoint Rust /api/projects/:id/deep-analyze.
class ProjectAnalyzeRequest(BaseModel):
    project_id: str
    project_name: str
    repo_summary: str = ""
    lang_hint: str = ""
    frameworks_list: list[str] = []
    config_files: list[dict] = []  # [{"path": "...", "content": "...", "truncated": bool}]
    registered_services: list[dict] = []
    # Provider preference (la prima disponibile vince). Se vuota, usa default chain.
    provider_chain: list[dict] = []  # [{"provider":"openai","model":"gpt-4o-mini"}, ...]


class AnalyzerChainUnavailable(Exception):
    """Sollevata quando la chain dei provider per l'analyzer non puo' essere
    letta dal DB (irraggiungibile o nexus_provider_default_model vuota).
    Il caller deve ritornare HTTP 503 con messaggio esplicito invece di
    applicare un fallback hardcoded."""
    pass


_ANALYZER_CHAIN_CACHE: list[dict] | None = None
_ANALYZER_CHAIN_CACHE_TS: float = 0.0


def _load_analyzer_provider_chain() -> list[dict]:
    """Carica la chain dei provider per l'analyzer da `nexus_provider_default_model`
    (vedi migrazione 0101) con cache 60s in-process.

    **Niente fallback hardcoded**. Se DB irraggiungibile o tabella vuota,
    solleva `AnalyzerChainUnavailable` con messaggio esplicito.
    """
    global _ANALYZER_CHAIN_CACHE, _ANALYZER_CHAIN_CACHE_TS
    import time
    now = time.time()
    if _ANALYZER_CHAIN_CACHE is not None and (now - _ANALYZER_CHAIN_CACHE_TS) < 60.0:
        return _ANALYZER_CHAIN_CACHE
    try:
        import psycopg2
        db_url = get_db_url()
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                # Ordine preferenziale per analyzer: economici/veloci prima,
                # capable per ultimo come fallback. L'ordine e' definito
                # dal seed in 0101 ma puo' essere personalizzato.
                cur.execute(
                    "SELECT provider, model_id FROM nexus_provider_default_model "
                    "ORDER BY CASE provider "
                    " WHEN 'openai' THEN 1 WHEN 'google' THEN 2 "
                    " WHEN 'deepseek' THEN 3 WHEN 'mistral' THEN 4 "
                    " WHEN 'anthropic' THEN 5 ELSE 99 END"
                )
                rows = cur.fetchall()
    except Exception as e:
        raise AnalyzerChainUnavailable(
            f"DB irraggiungibile: {e}. Verifica Postgres e migrazione 0101."
        )
    chain = [{"provider": p, "model": m} for (p, m) in rows]
    if not chain:
        raise AnalyzerChainUnavailable(
            "nexus_provider_default_model vuota. Applica la migrazione 0101 e popola la tabella."
        )
    _ANALYZER_CHAIN_CACHE = chain
    _ANALYZER_CHAIN_CACHE_TS = now
    return chain


def _load_project_analyzer_prompt() -> str | None:
    """Carica il template del prompt agent.project.analyzer dal DB.
    Ritorna None se non trovato.
    """
    try:
        import psycopg2
        db_url = get_db_url()
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT content FROM nexus_prompt_templates "
                    "WHERE key='agent.project.analyzer' AND is_active=TRUE "
                    "ORDER BY version DESC LIMIT 1"
                )
                row = cur.fetchone()
                return row[0] if row else None
    except Exception as e:
        logger.error("Errore caricamento prompt project.analyzer: %s", e)
        return None


def _render_analyzer_prompt(template: str, req: "ProjectAnalyzeRequest") -> str:
    """Sostituisce i placeholder {{...}} del template col payload del progetto.
    I file di config vengono serializzati in JSON compatto e inseriti come stringa.
    """
    config_payload = json_mod.dumps(
        [{"path": f.get("path",""), "content": f.get("content","")[:8000],
          "truncated": f.get("truncated", False)} for f in req.config_files],
        ensure_ascii=False,
    )
    services_payload = json_mod.dumps(req.registered_services, ensure_ascii=False)
    return (template
        .replace("{{lang_hint}}", req.lang_hint or "non determinato")
        .replace("{{frameworks_list}}", ", ".join(req.frameworks_list) if req.frameworks_list else "nessuno rilevato")
        .replace("{{repo_summary}}", req.repo_summary or f"progetto {req.project_name}")
        .replace("{{config_files_payload}}", config_payload)
        .replace("{{registered_services}}", services_payload)
    )


def _extract_json_block(text: str) -> dict | None:
    """Estrae il primo blocco JSON dal testo. Punto unico (regola L / ADR 0026):
    brain/utils/json_extract.extract_json_block."""
    from brain.utils.json_extract import extract_json_block

    return extract_json_block(text)


@router.post("/agent/project-analyze")
async def project_analyze(body: ProjectAnalyzeRequest) -> dict[str, object]:
    """Esegue l'agente agent.project.analyzer su un progetto.

    Pipeline:
      1. Carica template prompt da DB.
      2. Sostituisce placeholder col payload.
      3. Tenta i provider in ordine di preferenza (fallback chain).
      4. Parsa JSON dalla risposta.
      5. Ritorna {insights, model_used, duration_ms, status}.
    """
    started = time.time()
    template = _load_project_analyzer_prompt()
    if not template:
        return {
            "status": "failed",
            "error": "prompt agent.project.analyzer non trovato in DB",
            "insights": None, "model_used": None, "duration_ms": 0,
        }

    rendered = _render_analyzer_prompt(template, body)
    # Carica chain dal DB (nexus_provider_default_model, cache 60s).
    # Errore esplicito 503 se DB down o tabella vuota — niente fallback hardcoded.
    if body.provider_chain:
        chain = body.provider_chain
    else:
        try:
            chain = _load_analyzer_provider_chain()
        except AnalyzerChainUnavailable as e:
            return {
                "status": "failed",
                "error": str(e),
                "duration_ms": 0,
                "model_used": None,
                "insights": {},
            }

    last_error = None
    for entry in chain:
        prov = entry.get("provider", "")
        mdl  = entry.get("model", "")
        if not prov or not mdl:
            continue
        try:
            # Riusa la stessa pipeline del /complete. internal_task: task fuori
            # chat (project-analyze) — thinking off sui dual-mode (mig 0390).
            result = await runtime.providers.generate_completion_async(
                prov, mdl, rendered, internal_task=True,
            )
            content = (result.content or "").strip()
            if not content:
                last_error = f"{prov}/{mdl}: risposta vuota"
                continue
            parsed = _extract_json_block(content)
            if parsed is None:
                last_error = f"{prov}/{mdl}: output non parsabile come JSON"
                # prova provider successivo
                continue
            return {
                "status": "completed",
                "insights": parsed,
                "model_used": f"{prov}/{mdl}",
                "duration_ms": int((time.time() - started) * 1000),
                "raw_length": len(content),
            }
        except Exception as e:
            err_str = str(e)
            last_error = f"{prov}/{mdl}: {err_str[:200]}"
            logger.warning("project_analyze fallback su provider successivo (%s/%s): %s", prov, mdl, err_str[:200])
            continue

    return {
        "status": "failed",
        "error": last_error or "nessun provider disponibile",
        "insights": None,
        "model_used": None,
        "duration_ms": int((time.time() - started) * 1000),
    }


# ── Prompt revise: punto unico valuta+rivede un prompt vs direttive ─────────

def _prompt_applies_to_filter(prompt_key: str) -> list[str]:
    """Determina i valori applies_to delle guideline pertinenti al prompt."""
    if prompt_key.startswith("agent."):
        return ["all", "agent"]
    if prompt_key.startswith("system."):
        return ["all", "system"]
    if prompt_key.startswith("automation."):
        return ["all", "automation"]
    return ["all"]


def _load_active_guidelines(applies_to_values: list[str]) -> list[dict]:
    """Carica le direttive attive (is_active=TRUE) pertinenti dal DB.

    Ritorna lista di dict {practice_key, description, check_hint, severity}.
    Lista vuota se DB irraggiungibile (loggato; il chiamante decide come gestire).
    """
    try:
        import psycopg2
        db_url = get_db_url()
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT practice_key, description, check_hint, severity "
                    "FROM nexus_prompt_guideline "
                    "WHERE is_active = TRUE AND applies_to = ANY(%s) "
                    "ORDER BY CASE severity WHEN 'must' THEN 1 WHEN 'should' THEN 2 ELSE 3 END, practice_key",
                    (applies_to_values,),
                )
                rows = cur.fetchall()
    except Exception as e:
        logger.error("Errore caricamento direttive attive: %s", e)
        return []
    return [
        {"practice_key": r[0], "description": r[1], "check_hint": r[2], "severity": r[3]}
        for r in rows
    ]


class PromptReviseRequest(BaseModel):
    current_template: str
    prompt_key: str = ""
    mode: str = "evaluate"  # "evaluate" | "evaluate_and_revise"
    # signals: {"kind": "guideline"|"reflection", "weaknesses": [...], "guidelines": [...]}
    signals: dict = {}


@router.post("/agent/prompt-revise")
async def prompt_revise(body: PromptReviseRequest) -> dict[str, object]:
    """Punto unico (regola L): valuta ed eventualmente rivede un template prompt
    rispetto alle direttive attive. Usato da GuidelineAlignmentWorker,
    PromptOptimizerWorker e dalla UI admin.

    Pipeline:
      1. Carica le direttive attive pertinenti (da signals.guidelines o dal DB).
      2. Costruisce il prompt di valutazione/revisione (prompt_conformance_rubric).
      3. Risolve il modello via purpose tier-only 'prompt_conformance_check'.
      4. Chiama il provider, parsa il JSON, ritorna il risultato strutturato.
    """
    from brain.agents.prompt_conformance_rubric import build_revise_prompt, parse_revise_response
    from brain.router.service import _routing_client_singleton

    started = time.time()
    mode = body.mode if body.mode in ("evaluate", "evaluate_and_revise") else "evaluate"

    # 1. Direttive attive: usa quelle fornite o caricale dal DB
    guidelines = body.signals.get("guidelines") if isinstance(body.signals, dict) else None
    if not guidelines:
        guidelines = _load_active_guidelines(_prompt_applies_to_filter(body.prompt_key or ""))

    # 2. Prompt di valutazione/revisione
    system, user = build_revise_prompt(
        body.current_template, guidelines, mode=mode, signals=body.signals or None,
    )
    full_prompt = f"{system}\n\n{user}"

    # 3. Modello via purpose tier-only (niente fallback hardcoded, regola G)
    decision = _routing_client_singleton().purpose_model(purpose="prompt_conformance_check")
    if decision.provider.startswith("__"):
        return {
            "status": "failed",
            "error": f"purpose 'prompt_conformance_check' non risolvibile: {decision.rationale}",
            "duration_ms": int((time.time() - started) * 1000),
        }

    # 4. Chiamata provider + parse JSON robusto
    try:
        result = await runtime.providers.generate_completion_async(
            decision.provider, decision.model, full_prompt, internal_task=True,
        )
    except Exception as e:
        return {
            "status": "failed",
            "error": f"{decision.provider}/{decision.model}: {str(e)[:200]}",
            "duration_ms": int((time.time() - started) * 1000),
        }

    content = (result.content or "").strip()
    parsed = parse_revise_response(content, mode=mode)
    if parsed is None:
        return {
            "status": "failed",
            "error": "output non parsabile come JSON di conformita'",
            "model_used": f"{decision.provider}/{decision.model}",
            "duration_ms": int((time.time() - started) * 1000),
            "raw_length": len(content),
        }

    return {
        "status": "completed",
        "overall_score": parsed["overall_score"],
        "dimensions": parsed["dimensions"],
        "issues": parsed["issues"],
        "revised_template": parsed.get("revised_template"),
        "rationale": parsed.get("rationale"),
        "guideline_count": len(guidelines),
        "model_used": f"{decision.provider}/{decision.model}",
        "duration_ms": int((time.time() - started) * 1000),
    }


# ── Batch API (Anthropic Messages Batches) ─────────────────────────────────
class BatchAnalyzeRequest(BaseModel):
    requests: list[dict]  # [{"custom_id": str, "system": str, "prompt": str}]
    model: str | None = None  # Risolto da nexus_purpose_model 'anthropic_batch' se None
    max_tokens: int = 4096


@router.post("/batch-analyze/submit")
async def batch_analyze_submit(body: BatchAnalyzeRequest) -> dict[str, str]:
    # Ricaricare le credenziali dal DB per assicurarsi che siano aggiornate
    runtime._load_keys_from_db()

    from brain.providers.anthropic_batch import AnthropicBatchClient
    batch_id = await AnthropicBatchClient().submit_batch(body.requests, body.model, body.max_tokens)
    return {"batch_id": batch_id}


@router.get("/batch-analyze/{batch_id}/status")
async def batch_analyze_status(batch_id: str) -> dict[str, object]:
    from brain.providers.anthropic_batch import AnthropicBatchClient
    return await AnthropicBatchClient().poll_status(batch_id)


@router.get("/batch-analyze/{batch_id}/results")
async def batch_analyze_results(batch_id: str) -> list:
    from brain.providers.anthropic_batch import AnthropicBatchClient
    return await AnthropicBatchClient().get_results(batch_id)


# ── LangGraph Agent Endpoints ─────────────────────────────────────────────────

class AgentRunRequest(BaseModel):
    thread_id: str
    prompt: str
    behavior_mode: str = "bilanciata"
    # Modalita' agent-loop: se `tools_json` e' non vuoto, il grafo usa
    # `generate_agent_turn` e itera su tool_dispatch fino a end_turn.
    tools_json: list[dict] | None = None
    system_text: str = ""
    session_id: str | None = None
    provider_override: str | None = None
    model_override: str | None = None
    # Nome del profilo agente (core/github/specialized/general). Se None,
    # il router sceglie un profilo a partire dall'intent; "none" disabilita.
    profile_name: str | None = None
    conversation_history: list[dict] | None = None
    run_id: str | None = None
    # Modalita' automazione del turno chat propagata da mcp-core.
    # Valori attesi: "none" | "confirm" | "automatic" | "continuous".
    # Letta da clarify_or_expand_node per skip in modalita' autonoma.
    automation_mode: str | None = None
    # Intent gia' RISOLTO a monte da mcp-core (oggi: risposta dell'utente a una
    # disambiguazione "A"/"B"/"C", resolve_disambiguation_reply). Quando
    # presente, router_node lo usa al posto di ri-classificare il prompt: la
    # lettera secca verrebbe ri-marcata 'chat' (prompt_len=1) perdendo la
    # scelta dell'utente. None = classificazione normale.
    intent_hint: str | None = None


class AgentFeedbackRequest(BaseModel):
    score: float


@router.post("/agent/run")
async def agent_run(body: AgentRunRequest) -> dict[str, object]:
    """Avvia un'esecuzione dell'agent LangGraph.

    Il grafo si ferma prima di `executor` (human-in-the-loop).
    Risponde con status "pending_approval" finché non si chiama /agent/approve.

    Nel response completato include le metriche estese: token, costo, latency.
    """
    from langchain_core.messages import HumanMessage as _HumanMessage

    graph = runtime._get_agent_graph()
    config: dict[str, object] = {"configurable": {"thread_id": body.thread_id}}

    # Punto unico history builder (regola L, S72).
    history_msgs = _build_history_messages(body.conversation_history)

    initial_state = {
        "messages": history_msgs + [_HumanMessage(content=body.prompt)],
        "behavior_mode": body.behavior_mode,
        "thread_id": body.thread_id,
        "iterations": 0,
        "result": None,
        "provider_used": None,
        "model_used": None,
        "feedback_score": None,
        "latency_ms": None,
        "token_usage": None,
        "tools_json": body.tools_json or [],
        "system_text": body.system_text or "",
        "session_id": body.session_id,
        "provider_override": body.provider_override,
        "model_override": body.model_override,
        "profile_name": body.profile_name,
        "pending_tool_uses": [],
        "stop_reason": None,
        "approved": False,
        # Reset clarify per run (mig 0386): ogni run parte senza clarify pendente
        # e con contatore azzerato, indipendentemente dal checkpointer.
        "pending_clarify": False,
        "clarify_attempts": 0,
        "intent_hint": body.intent_hint,
        "prompt_tokens": None,
        "completion_tokens": None,
        "cache_creation_tokens": None,
        "cache_read_tokens": None,
        "total_tokens": None,
        "total_cost_usd": None,
        "cache_hit_rate": None,
        "temperature": None,
        "top_p": None,
        "created_at": None,
        "completed_at": None,
    }
    try:
        result = await graph.ainvoke(initial_state, config=config)  # type: ignore[union-attr]
        # Skip get_state() se non abbiamo un checkpointer (causa deadlock AsyncSqliteSaver)
        next_nodes = None
        try:
            next_nodes = graph.get_state(config).next  # type: ignore[union-attr]
        except Exception:
            pass
        if next_nodes:
            return {
                "status": "pending_approval",
                "thread_id": body.thread_id,
                "next": list(next_nodes),
                "user_intent": result.get("user_intent"),
                "task_type": result.get("task_type"),
                "routing_mode": result.get("behavior_mode"),
            }
        return {
            "status": "completed",
            "thread_id": body.thread_id,
            "result": result.get("result"),
            "provider_used": result.get("provider_used"),
            "model_used": result.get("model_used"),
            "latency_ms": result.get("latency_ms"),
            "usage": {
                "promptTokens": result.get("prompt_tokens") or 0,
                "completionTokens": result.get("completion_tokens") or 0,
                "cacheCreationTokens": result.get("cache_creation_tokens") or 0,
                "cacheReadTokens": result.get("cache_read_tokens") or 0,
                "totalTokens": result.get("total_tokens") or 0,
            },
            "totalCostUsd": result.get("total_cost_usd") or 0.0,
            "cacheHitRate": result.get("cache_hit_rate") or 0.0,
            "temperature": result.get("temperature"),
            "topP": result.get("top_p"),
            "createdAt": result.get("created_at"),
            "completedAt": result.get("completed_at"),
        }
    except Exception as exc:
        logger.error("agent_run error: %s", exc)
        return {"status": "error", "detail": str(exc)}


@router.post("/agent/approve/{thread_id}")
async def agent_approve(thread_id: str) -> dict[str, object]:
    """Riprende l'esecuzione dell'agent dal checkpoint (human approval).

    Include metriche estese nel response.
    """
    graph = runtime._get_agent_graph()
    config: dict[str, object] = {"configurable": {"thread_id": thread_id}}
    try:
        result = await graph.ainvoke(None, config=config)  # type: ignore[union-attr]
        return {
            "status": "completed",
            "thread_id": thread_id,
            "result": result.get("result"),
            "provider_used": result.get("provider_used"),
            "model_used": result.get("model_used"),
            "latency_ms": result.get("latency_ms"),
            "token_usage": result.get("token_usage"),
            "usage": {
                "promptTokens": result.get("prompt_tokens") or 0,
                "completionTokens": result.get("completion_tokens") or 0,
                "cacheCreationTokens": result.get("cache_creation_tokens") or 0,
                "cacheReadTokens": result.get("cache_read_tokens") or 0,
                "totalTokens": result.get("total_tokens") or 0,
            },
            "totalCostUsd": result.get("total_cost_usd") or 0.0,
            "cacheHitRate": result.get("cache_hit_rate") or 0.0,
            "temperature": result.get("temperature"),
            "topP": result.get("top_p"),
            "createdAt": result.get("created_at"),
            "completedAt": result.get("completed_at"),
        }
    except Exception as exc:
        logger.error("agent_approve error: %s", exc)
        return {"status": "error", "detail": str(exc)}


@router.get("/agent/state/{thread_id}")
async def agent_state(thread_id: str) -> dict[str, object]:
    """Recupera lo snapshot di stato del grafo per un thread."""
    graph = runtime._get_agent_graph()
    config: dict[str, object] = {"configurable": {"thread_id": thread_id}}
    try:
        snapshot = graph.get_state(config)  # type: ignore[union-attr]
        return {
            "thread_id": thread_id,
            "next": list(snapshot.next) if snapshot.next else [],
            "values": {
                k: v for k, v in (snapshot.values or {}).items()
                if k not in ("messages",)
            },
        }
    except Exception as exc:
        logger.error("agent_state error: %s", exc)
        return {"status": "error", "detail": str(exc)}


@router.post("/agent/feedback/{thread_id}")
async def agent_feedback(thread_id: str, body: AgentFeedbackRequest) -> dict[str, object]:
    """Registra il feedback utente per l'ultima interazione di un thread."""
    try:
        from brain.memory.storage import PostgresLearningStorage

        storage = PostgresLearningStorage()
        updated = storage.update_feedback(thread_id, body.score)
        return {"thread_id": thread_id, "updated": updated, "score": body.score}
    except Exception as exc:
        logger.error("agent_feedback error: %s", exc)
        return {"status": "error", "detail": str(exc)}


@router.get("/agent/stats")
async def agent_stats() -> dict[str, object]:
    """Restituisce statistiche aggregate sulle interazioni per tipo di task."""
    try:
        from brain.memory.storage import PostgresLearningStorage

        storage = PostgresLearningStorage()
        return {"stats": storage.get_task_stats()}
    except Exception as exc:
        logger.error("agent_stats error: %s", exc)
        return {"status": "error", "detail": str(exc)}


# ── Streaming token (SSE) ──────────────────────────────────────────────────
class AgentTurnStreamRequest(BaseModel):
    provider: str
    model: str
    messages_json: str
    tools_json: str
    max_tokens: int = 8192
    system_text: str = ""


@router.post("/agent-turn/stream")
async def agent_turn_stream(body: AgentTurnStreamRequest) -> StreamingResponse:
    import json as _json

    async def generate():
        try:
            # Ricaricare le credenziali dal DB per assicurarsi che siano aggiornate
            runtime._load_keys_from_db()

            prov = runtime.providers.get_provider(body.provider)
            if prov is None:
                yield f"data: {_json.dumps({'type': 'error', 'message': f'Provider {body.provider} non trovato'})}\n\n"
                return
            if not hasattr(prov, "generate_agent_turn_stream"):
                yield f"data: {_json.dumps({'type': 'error', 'message': f'Provider {body.provider} non supporta lo streaming'})}\n\n"
                return
            messages = _json.loads(body.messages_json)
            tools = _json.loads(body.tools_json)
            async for chunk in prov.generate_agent_turn_stream(
                body.model, messages, tools, body.max_tokens, body.system_text
            ):
                yield f"data: {_json.dumps(chunk)}\n\n"
        except Exception as exc:
            yield f"data: {_json.dumps({'type': 'error', 'message': str(exc)})}\n\n"

    return StreamingResponse(generate(), media_type="text/event-stream")


# ── Streaming del grafo LangGraph (agent-loop) ─────────────────────────────
@router.post("/agent/run/stream")
async def agent_run_stream(body: AgentRunRequest) -> StreamingResponse:
    """Esegue il grafo agent streamando eventi SSE ad ogni transizione.

    Event types emessi:
      - assistant_delta : contenuto testuale parziale dell'assistant
      - tool_use        : l'LLM ha richiesto un tool (name, input, tool_use_id)
      - tool_result     : output del ToolRunner (tool_use_id, content, is_error)
      - end_turn        : il modello ha terminato (stop_reason=end_turn)
      - error           : errore di esecuzione
    """
    import json as _json

    from langchain_core.messages import HumanMessage as _HumanMessage

    graph = runtime._get_agent_graph()
    config: dict[str, object] = {"configurable": {"thread_id": body.thread_id}}

    # Punto unico history builder (regola L, S72).
    history_msgs = _build_history_messages(body.conversation_history)

    initial_state = {
        "messages": history_msgs + [_HumanMessage(content=body.prompt)],
        "behavior_mode": body.behavior_mode,
        "thread_id": body.thread_id,
        "iterations": 0,
        "tools_json": body.tools_json or [],
        "system_text": body.system_text or "",
        "session_id": body.session_id,
        "provider_override": body.provider_override,
        "model_override": body.model_override,
        "profile_name": body.profile_name,
        "automation_mode": body.automation_mode,
        "pending_tool_uses": [],
        "stop_reason": None,
        "approved": False,
        # Reset clarify per run (mig 0386): ogni run parte senza clarify pendente
        # e con contatore azzerato, indipendentemente dal checkpointer.
        "pending_clarify": False,
        "clarify_attempts": 0,
        "intent_hint": body.intent_hint,
    }

    async def generate():
        # Accumulatori token/costo tra tutte le iterazioni dell'agente
        acc_prompt_tokens = 0
        acc_completion_tokens = 0
        acc_total_tokens = 0
        acc_total_cost = 0.0
        end_turn_emitted = False
        done_emitted = False
        # Metadata routing (B5): catturati dal nodo router per propagare a Rust
        nexus_task_type: str | None = None
        nexus_agent_type: str | None = None
        # Segnali macchina a stati di terminazione (mig 0386), propagati a mcp-core
        # nell'evento end_turn:
        #  - forced_close_unverified: abort anti-loop -> FailedDiagnosed (non
        #    Completed, anche se final_gate riscrive stop_reason a "end_turn").
        #  - final_gate_passed: verifica E2E superata -> CompletedVerified.
        forced_close_unverified = False
        final_gate_passed = False
        declared_outcome = None
        tool_infra_error = False
        # Provider/model EFFETTIVI dell'ultima iterazione executor: catturano il
        # cascade fallback sticky intra-run (es. deepseek -> google/gemini-2.5-pro).
        # Propagati nell'evento end_turn cosi' mcp-core salva su agent_runs il
        # modello reale che ha prodotto la risposta finale, non quello iniziale
        # della routing decision (vedi brain_agent_client.rs::run_via_brain).
        effective_provider: str | None = None
        effective_model: str | None = None
        try:
            # Heartbeat: ogni attesa di evento ha un timeout di 30s.
            # Se il brain e' in elaborazione senza produrre output (tool lento,
            # LLM streaming lento, attesa gRPC), emette un ping SSE per segnalare
            # a mcp-core Rust che il run e' ancora attivo.
            # Cosi' mcp-core puo' usare un timeout per-silence (120s) invece del
            # timeout monolitico fisso sulla connessione SSE.
            # stream_mode=["updates","custom"]: riceviamo sia i delta
            # finali dei nodi (mode="updates") che gli eventi push emessi
            # in tempo reale da _stream_thinking_live (mode="custom").
            # Quando stream_mode e una lista, ogni evento e una tupla
            # (mode, payload). Vedi nodes.py::_stream_thinking_live e
            # ADR 0013 per il razionale (streaming live thinking).
            _aiter = graph.astream(  # type: ignore[union-attr]
                initial_state,
                config=config,
                stream_mode=["updates", "custom"],
            ).__aiter__()
            while True:
                try:
                    raw_event = await asyncio.wait_for(_aiter.__anext__(), timeout=30.0)
                except asyncio.TimeoutError:
                    yield 'data: {"type":"ping"}\n\n'
                    continue
                except StopAsyncIteration:
                    break

                # Quando stream_mode e lista, l evento e (mode, payload).
                # Fallback difensivo se per qualche motivo arriva il dict
                # nudo (es. versioni vecchie di LangGraph): trattalo come updates.
                if isinstance(raw_event, tuple) and len(raw_event) == 2:
                    _mode, _payload = raw_event
                else:
                    _mode, _payload = "updates", raw_event

                # Mode "custom": eventi push emessi da _stream_thinking_live
                # (e in futuro da altri helper). Convertiamo immediatamente
                # in SSE thinking_delta senza aspettare il return del nodo.
                if _mode == "custom":
                    if isinstance(_payload, dict) and _payload.get("kind") == "nexus_thinking":
                        _txt = str(_payload.get("text") or "").strip()
                        if _txt:
                            yield (
                                "data: "
                                + _json.dumps({"type": "thinking_delta", "text": _txt})
                                + "\n\n"
                            )
                    continue

                # Mode "updates": delta dict per nodo finito (comportamento storico).
                event = _payload
                if not isinstance(event, dict):
                    continue
                _learner_seen = False
                for node, delta in event.items():
                    if node == "learner":
                        _learner_seen = True
                    if not isinstance(delta, dict):
                        continue
                    # ── Meta-step pubblicati da QUALUNQUE nodo ─────────────
                    # I nodi (planner, router, executor su fallback, ecc.)
                    # possono accodare entry strutturate in `delta["meta_steps"]`
                    # via brain/agents/meta_steps.make(). Il generator le
                    # converte in eventi SSE `{"type":"meta_step", ...}`
                    # consumati da mcp-core::brain_agent_client.
                    for ms in (delta.get("meta_steps") or []):
                        if not isinstance(ms, dict) or not ms.get("kind"):
                            continue
                        ms_payload = {
                            "type": "meta_step",
                            "kind": ms["kind"],
                            "title": ms.get("title", ""),
                            "payload": ms.get("payload") or {},
                            "created_at": ms.get("created_at"),
                        }
                        if ms.get("correlation_id"):
                            ms_payload["correlation_id"] = ms["correlation_id"]
                        logger.info("SSE meta_step emit: node=%s kind=%s title=%s", node, ms["kind"], (ms.get("title") or "")[:60])
                        yield "data: " + _json.dumps(ms_payload) + "\n\n"
                    # Nexus thinking: i nodi possono popolare `nexus_thinking`
                    # come list[str] (preferito) o singola str. Lo convertiamo
                    # in eventi SSE `thinking_delta` che mcp-core inoltra al
                    # frontend per visualizzare il ragionamento dell'agente.
                    _thinking = delta.get("nexus_thinking")
                    if _thinking:
                        logger.info("SSE thinking_delta emit: node=%s n=%s", node, len(_thinking) if isinstance(_thinking, list) else 1)
                    if isinstance(_thinking, list):
                        for _line in _thinking:
                            if not _line:
                                continue
                            _txt = str(_line).strip()
                            if not _txt:
                                continue
                            yield (
                                "data: "
                                + _json.dumps({"type": "thinking_delta", "text": _txt})
                                + "\n\n"
                            )
                    elif isinstance(_thinking, str) and _thinking.strip():
                        yield (
                            "data: "
                            + _json.dumps({"type": "thinking_delta", "text": _thinking.strip()})
                            + "\n\n"
                        )
                    # Punto unico di cattura dei segnali di terminazione (mig 0386):
                    # da qualunque nodo li emetta (executor abort / final_gate pass),
                    # cosi' l'esito e' corretto indipendentemente da quale nodo chiude.
                    if delta.get("forced_close_unverified"):
                        forced_close_unverified = True
                    if delta.get("final_gate_passed"):
                        final_gate_passed = True
                    # WAVE 3.2: esito DICHIARATO dal modello (task_complete),
                    # propagato a mcp-core per derivare lo status canonico
                    # (blocked/needs_input -> BlockedNeedsInput) e usare il
                    # summary come risposta se il modello non ha prodotto testo.
                    if delta.get("declared_outcome"):
                        declared_outcome = delta["declared_outcome"]
                    # WAVE 2.2: errore infrastruttura tool (ToolRunner down).
                    if delta.get("tool_infra_error"):
                        tool_infra_error = True
                    if node == "router":
                        # Cattura metadata routing (B5 fix: propagazione nexus_task_type/agent_type)
                        if delta.get("user_intent"):
                            nexus_task_type = delta["user_intent"]
                        if delta.get("profile_name"):
                            nexus_agent_type = delta["profile_name"]
                    elif node == "executor":
                        # Accumula token/costo da ogni chiamata LLM
                        acc_prompt_tokens += int(delta.get("prompt_tokens") or 0)
                        acc_completion_tokens += int(delta.get("completion_tokens") or 0)
                        acc_total_tokens += int(delta.get("total_tokens") or 0)
                        acc_total_cost += float(delta.get("total_cost_usd") or 0.0)
                        # Cattura provider/model effettivi dell'iterazione corrente
                        # (riflettono il cascade fallback sticky se avvenuto).
                        if delta.get("provider_used"):
                            effective_provider = delta["provider_used"]
                        if delta.get("model_used"):
                            effective_model = delta["model_used"]

                        # Token live: emette i token cumulativi a ogni iterazione
                        # executor, cosi' la barra context della UI si aggiorna in
                        # tempo reale senza attendere end_turn (event-driven, niente
                        # polling). mcp-core lo ritrasmette come `agent_usage`.
                        # `last_prompt_tokens` = prompt dell'iterazione corrente,
                        # usato dal frontend per il ratio di riempimento context.
                        if acc_total_tokens > 0:
                            yield (
                                "data: "
                                + _json.dumps({
                                    "type": "usage",
                                    "prompt_tokens": acc_prompt_tokens,
                                    "completion_tokens": acc_completion_tokens,
                                    "total_tokens": acc_total_tokens,
                                    "total_cost": acc_total_cost,
                                    "last_prompt_tokens": int(delta.get("prompt_tokens") or 0),
                                })
                                + "\n\n"
                            )

                        result_text = delta.get("result") or ""
                        if result_text:
                            yield (
                                "data: "
                                + _json.dumps({
                                    "type": "assistant_delta",
                                    "text": result_text,
                                })
                                + "\n\n"
                            )
                        for tu in (delta.get("pending_tool_uses") or []):
                            yield (
                                "data: "
                                + _json.dumps({
                                    "type": "tool_use",
                                    "tool_use_id": tu.get("id"),
                                    "name": tu.get("name"),
                                    "input": tu.get("input"),
                                })
                                + "\n\n"
                            )
                        if delta.get("stop_reason") == "end_turn":
                            end_turn_emitted = True
                            end_turn_payload = {
                                "type": "end_turn",
                                "prompt_tokens": acc_prompt_tokens,
                                "completion_tokens": acc_completion_tokens,
                                "total_tokens": acc_total_tokens,
                                "total_cost": acc_total_cost,
                            }
                            # B5: propaga metadata routing a mcp-core Rust
                            if nexus_task_type:
                                end_turn_payload["nexus_task_type"] = nexus_task_type
                            if nexus_agent_type:
                                end_turn_payload["nexus_agent_type"] = nexus_agent_type
                            if forced_close_unverified:
                                end_turn_payload["forced_close_unverified"] = True
                            if final_gate_passed:
                                end_turn_payload["final_gate_passed"] = True
                            if declared_outcome:
                                end_turn_payload["declared_outcome"] = declared_outcome
                            if tool_infra_error:
                                end_turn_payload["error_class"] = "infrastructure"
                            # Provider/model effettivi (cascade fallback sticky):
                            # mcp-core li usa per salvare il modello reale nel
                            # messaggio assistant invece di quello iniziale.
                            if effective_provider:
                                end_turn_payload["provider_used"] = effective_provider
                            if effective_model:
                                end_turn_payload["model_used"] = effective_model
                            yield (
                                "data: "
                                + _json.dumps(end_turn_payload)
                                + "\n\n"
                            )
                    elif node == "tool_dispatch":
                        # L'ultimo HumanMessage aggiunto contiene i tool_result.
                        for msg in (delta.get("messages") or []):
                            extra = getattr(msg, "additional_kwargs", {}) or {}
                            for block in (extra.get("anthropic_content") or []):
                                if isinstance(block, dict) and block.get("type") == "tool_result":
                                    yield (
                                        "data: "
                                        + _json.dumps({
                                            "type": "tool_result",
                                            "tool_use_id": block.get("tool_use_id"),
                                            "content": block.get("content"),
                                            "is_error": bool(block.get("is_error")),
                                        })
                                        + "\n\n"
                                    )
                # Se questo era l'evento del learner_node (ultimo del graph),
                # emettiamo subito `end_turn` (se non gia' emesso) e `done`.
                # Risolve il caso in cui astream() non emette StopAsyncIteration
                # tempestivamente dopo aver consumato l'ultimo nodo del graph.
                if _learner_seen:
                    if not end_turn_emitted and acc_total_tokens > 0:
                        end_turn_emitted = True
                        _final_payload = {
                            "type": "end_turn",
                            "prompt_tokens": acc_prompt_tokens,
                            "completion_tokens": acc_completion_tokens,
                            "total_tokens": acc_total_tokens,
                            "total_cost": acc_total_cost,
                        }
                        if nexus_task_type:
                            _final_payload["nexus_task_type"] = nexus_task_type
                        if nexus_agent_type:
                            _final_payload["nexus_agent_type"] = nexus_agent_type
                        if forced_close_unverified:
                            _final_payload["forced_close_unverified"] = True
                        if final_gate_passed:
                            _final_payload["final_gate_passed"] = True
                        if declared_outcome:
                            _final_payload["declared_outcome"] = declared_outcome
                        if tool_infra_error:
                            _final_payload["error_class"] = "infrastructure"
                        if effective_provider:
                            _final_payload["provider_used"] = effective_provider
                        if effective_model:
                            _final_payload["model_used"] = effective_model
                        yield "data: " + _json.dumps(_final_payload) + "\n\n"
                    yield 'data: {"type":"done"}\n\n'
                    done_emitted = True
                    break
        except asyncio.CancelledError:
            # Il client mcp-core ha chiuso la connessione TCP. NON dobbiamo
            # mascherare la cancellazione (FastAPI ha bisogno di propagarla
            # per il cleanup), ma il blocco `finally` sotto garantisce che
            # `done` venga comunque emesso prima che il generator chiuda —
            # questo previene il caso di mcp-core in attesa per 120s del
            # timeout di silenzio quando lo stream e' gia' morto.
            logger.warning("agent_run_stream cancellato dal client (CancelledError)")
            raise
        except Exception as exc:
            import traceback as _tb
            logger.error("agent_run_stream error: %s\n%s", exc, _tb.format_exc())
            # Classifichiamo l'eccezione per propagare error_class strutturato a mcp-core
            # (vedi crates/mcp-core/src/brain_agent_client.rs::classify_provider_error
            # che lo legge come fonte primaria invece di pattern-matchare la stringa).
            try:
                from brain.providers.error_handler import classify_error as _classify
                _info = _classify(exc, body.provider if hasattr(body, 'provider') else "unknown")
                _err_class = _info.get("stop_reason")
                _retry_after = _info.get("retry_after_seconds")
            except Exception:
                _err_class = None
                _retry_after = None
            _payload = {
                "type": "error",
                "message": str(exc) or repr(exc),
            }
            if _err_class:
                _payload["error_class"] = _err_class
            if _retry_after is not None:
                _payload["retry_after_seconds"] = _retry_after
            yield f"data: {_json.dumps(_payload)}\n\n"
        finally:
            # Garanzia di chiusura SSE: `done` deve essere SEMPRE emesso
            # (anche su CancelledError, eccezione, o uscita normale del loop)
            # affinche' mcp-core possa chiudere il proprio stream senza
            # aspettare il timeout di silenzio (120s).
            if not end_turn_emitted and acc_total_tokens > 0:
                try:
                    yield (
                        "data: "
                        + _json.dumps({
                            "type": "end_turn",
                            "prompt_tokens": acc_prompt_tokens,
                            "completion_tokens": acc_completion_tokens,
                            "total_tokens": acc_total_tokens,
                            "total_cost": acc_total_cost,
                        })
                        + "\n\n"
                    )
                except (GeneratorExit, asyncio.CancelledError):
                    # Client gia' chiuso, non possiamo piu' yield
                    pass
            if not done_emitted:
                try:
                    yield 'data: {"type":"done"}\n\n'
                except (GeneratorExit, asyncio.CancelledError):
                    pass

    return StreamingResponse(generate(), media_type="text/event-stream")
