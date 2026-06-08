"""Implementazione dei nodi del grafo LangGraph per Nexus.

Ogni nodo riceve AgentState e restituisce un dict con i campi aggiornati.
I nodi usano i servizi globali di Nexus (providers, router, embeddings)
invece di dipendenze esterne come LiteLLM.
"""
from __future__ import annotations

import asyncio
import hashlib
import json
import logging
import os
import random
import time
import uuid
from typing import TYPE_CHECKING, Any

from langchain_core.messages import AIMessage, HumanMessage, ToolMessage

from brain.utils.db_pool import get_db_url
from .. import (
    meta_steps,
    profile_loader,
    prompt_registry,
    prompt_renderer,
    reflection_config,
    orchestrator_config,
    todo_store,
)
from ..reflection_rubric import build_reflection_prompt, parse_reflection_response
from ..state import AgentState

if TYPE_CHECKING:
    from brain.embeddings import EmbeddingService
    from brain.grpc_clients.agent_router_client import AgentRouterClient
    from brain.grpc_clients.tool_runner_client import ToolRunnerClient
    from brain.memory.retrieval import InteractionRetriever
    from brain.memory.storage import PostgresLearningStorage as LocalLearningStorage
    from brain.providers import ProviderRegistry
    from brain.router import SemanticRouter

logger = logging.getLogger(__name__)

# Refactoring god-file: gli helper PURI vivono in .helpers, le route in .routing.
# Import esplicito di tutti i simboli helper (inclusi quelli con underscore, che
# 'import *' non porterebbe) cosi' i nodi di questo modulo li usano invariati.
from .helpers import (
    MAX_AGENT_ITERATIONS,
    _ADAPTIVE_BUDGET_CACHE,
    _ADAPTIVE_BUDGET_TTL_SEC,
    _ADAPTIVE_BUDGET_DEFAULTS,
    _WEAK_MODELS_HINT,
    _STEP_MARKER_RE,
    _FILE_PATH_RE,
    _load_adaptive_budget_config,
    _G1_NUDGE_CACHE,
    _G1_NUDGE_TTL_SEC,
    _G1_NUDGE_DEFAULT_MAX,
    _load_g1_max_nudges,
    _EXPLORATION_ONLY_TOOLS,
    _EXPLORATION_LOOP_CACHE,
    _EXPLORATION_LOOP_TTL_SEC,
    _EXPLORATION_LOOP_DEFAULT,
    _load_exploration_loop_threshold,
    _LANG_REMINDER_MARKER,
    _LANG_REMINDER_CACHE,
    _LANG_REMINDER_TTL_SEC,
    _LANG_REMINDER_DEFAULT_ENABLED,
    _LANG_REMINDER_DEFAULT_TEXT,
    _load_language_reminder,
    _inject_forced_rag_reminder,
    _inject_language_reminder,
    estimate_prompt_complexity,
    compute_iteration_budget,
    apply_agentic_tier_floor,
    _NEXUS_THINKING_CACHE,
    _NEXUS_THINKING_TTL_SEC,
    _nexus_thinking_enabled,
    _STREAM_WRITER_DIAG,
    _stream_thinking_live,
    _emit_thinking,
    _describe_tool_call,
    _ACTION_PATTERNS,
    _detect_action_request,
    _INTENT_NARRATION_PATTERNS,
    _detect_unfulfilled_intent,
    _detect_polling_wait,
    build_unfulfilled_report,
    _last_assistant_text,
    _pick_escalation_model,
    _has_tool_calls_in_history,
    _TOOL_ERROR_HINTS,
    _detect_repeated_failed_command,
    _detect_recent_tool_error,
    _PRICE_CACHE,
    _PRICE_CACHE_TS,
    _PRICE_TTL_S,
    _lookup_price,
    MAX_TOOL_RESULT_CHARS,
    MAX_CONTEXT_CHARS,
    _smart_truncate,
    _estimate_context_chars,
    _estimate_context_tokens,
    _dedup_tool_results,
    _first_human_index,
    _is_summary_message,
    _CTX_MGMT_CACHE,
    _CTX_MGMT_TTL_SEC,
    _CTX_MGMT_DEFAULTS,
    _load_ctx_mgmt_config,
    _should_compress_now,
    _tool_use_signature,
    _dedup_tool_results_history,
    _BASE64_RE,
    _looks_like_base64,
    _drop_unused_base64_payloads,
    _CONTEXT_WINDOW_CACHE,
    _CONTEXT_WINDOW_TTL_SEC,
    _model_context_window,
    _offload_system_prompt_if_huge,
    _apply_rolling_summary,
    _provider_from_model,
    _smart_upscale_model,
    _estimate_tool_result_size_bytes,
    _current_context_token_estimate,
    _predictive_cap_check,
    _langchain_to_anthropic_messages,
    _ATTACHMENT_BUDGET_CACHE,
    _ATTACHMENT_BUDGET_TTL,
    _attachment_budget_bytes,
    _ATTACHMENT_READ_TOOLS,
    _extract_returned_bytes,
)
from .routing import (
    route_after_executor,
    route_after_regression_gate,
    route_after_verifier,
    route_by_task_type,
)

logger = logging.getLogger(__name__)

def _smart_truncate_lossless(
    text: str,
    max_chars: int = MAX_TOOL_RESULT_CHARS,
    *,
    source_kind: str = "tool_result",
    metadata: dict[str, Any] | None = None,
) -> str:
    """Variante LOSSLESS di _smart_truncate: prima di tagliare offloada in RAG.

    Se il testo supera `max_chars`, il contenuto COMPLETO viene indicizzato in
    Qdrant (collection tool_results_chunks) via context_offload, poi nel prompt
    resta testa+coda + un puntatore esplicito a `nexus_search_semantic`. Cosi'
    il dato non viene MAI perso a livello di sistema (regola H: niente
    troncamento distruttivo).

    Best-effort: se l'offload non e' disponibile (Qdrant down), degrada al
    troncamento classico ma con puntatore che lo segnala (build_pointer).
    """
    if len(text) <= max_chars:
        return text
    from .. import context_offload

    offload = context_offload.offload_to_rag(
        _embeddings, text, source_kind=source_kind, metadata=metadata,
    )
    head_size = max_chars // 5
    tail_size = max(200, max_chars - head_size - 200)
    pointer = context_offload.build_pointer(len(text), offload)
    return text[:head_size] + pointer + text[-tail_size:]


# ── Configurazione self-reflection (Fase 2) ──────────────────────────────────
# Tutti i parametri sono letti ESCLUSIVAMENTE dal DB tramite reflection_config.
# Nessuna variabile d'ambiente: le modifiche admin sono attive entro 60 secondi
# senza rideploy del servizio.

# Riferimenti ai servizi globali — iniettati da graph.py dopo l'inizializzazione
_providers: ProviderRegistry | None = None
_router: SemanticRouter | None = None
_embeddings: EmbeddingService | None = None
_storage: LocalLearningStorage | None = None
_retriever: InteractionRetriever | None = None
_tool_runner: ToolRunnerClient | None = None
_agent_router: AgentRouterClient | None = None
# PR-D: classifier agentico LLM per il gating adattivo del planner forte.
_agentic_classifier: Any = None

# ── Learning config (DB-backed, cache 60s) ───────────────────────────────────
# Letti dalla tabella `settings`: learning_auto_extract e learning_min_confidence.
# Stessa convenzione di reflection_config: nessuna env var, nessun hardcode,
# fallback conservativo se DB non raggiungibile.
import threading as _threading

_learning_cfg_lock = _threading.Lock()
_learning_cfg_cache: dict[str, Any] | None = None
_learning_cfg_ts: float = 0.0
_LEARNING_CFG_TTL = 60.0
_LEARNING_CFG_DEFAULTS: dict[str, Any] = {
    "auto_extract": True,
    "min_confidence": 0.6,
}


def _get_learning_config() -> dict[str, Any]:
    """Restituisce {auto_extract: bool, min_confidence: float} dal DB con cache 60s.

    Se DB non raggiungibile o psycopg2 mancante, usa i safe_defaults conservativi
    (auto_extract=True, min_confidence=0.6) e mantiene l'ultima cache valida.
    """
    global _learning_cfg_cache, _learning_cfg_ts
    import os

    now = time.monotonic()
    with _learning_cfg_lock:
        if _learning_cfg_cache is not None and now - _learning_cfg_ts < _LEARNING_CFG_TTL:
            return _learning_cfg_cache

    database_url = os.environ.get("DATABASE_URL")
    if not database_url:
        logger.warning("learning_config: DATABASE_URL non impostato, uso safe_defaults")
        with _learning_cfg_lock:
            _learning_cfg_cache = dict(_LEARNING_CFG_DEFAULTS)
            _learning_cfg_ts = now
        return dict(_LEARNING_CFG_DEFAULTS)

    try:
        import psycopg2  # type: ignore[import-untyped]
        conn = psycopg2.connect(database_url)
        try:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT key, value FROM settings "
                    "WHERE key IN ('learning_auto_extract','learning_min_confidence')"
                )
                rows = dict(cur.fetchall())
        finally:
            conn.close()
        cfg: dict[str, Any] = {
            "auto_extract": rows.get("learning_auto_extract", "true").strip().lower() != "false",
            "min_confidence": float(rows.get("learning_min_confidence", "0.6") or "0.6"),
        }
    except Exception as exc:
        logger.error("learning_config: errore lettura DB: %s — uso cache o defaults", exc)
        with _learning_cfg_lock:
            return dict(_learning_cfg_cache) if _learning_cfg_cache is not None else dict(_LEARNING_CFG_DEFAULTS)

    with _learning_cfg_lock:
        _learning_cfg_cache = cfg
        _learning_cfg_ts = now
    return cfg


def configure_services(
    providers: Any,
    router: Any,
    embeddings: Any,
    storage: Any,
    retriever: Any,
    tool_runner: Any = None,
    agent_router: Any = None,
    agentic_classifier: Any = None,
) -> None:
    """Inietta i servizi globali nei nodi. Chiamato da create_agent_graph()."""
    global _providers, _router, _embeddings, _storage, _retriever
    global _tool_runner, _agent_router, _agentic_classifier
    _providers = providers
    _router = router
    _embeddings = embeddings
    _storage = storage
    _retriever = retriever
    _tool_runner = tool_runner
    _agent_router = agent_router
    _agentic_classifier = agentic_classifier


# ─── RAG helper (BP7) ───────────────────────────────────────────────────────

# Intent per i quali il RAG inline e' utile: task che operano su codice/repo
# e tipicamente beneficiano del ricordo di task simili passati.
# Intent per cui si attiva il retrieve della memoria di progetto (RAG su
# chat_messages + Knowledge Base). Include i task agentici autonomi
# (`agentic_default`) e la diagnostica servizi (`system_admin`): sono proprio
# i task complessi che traggono piu' beneficio dalla memoria del progetto.
# Senza di essi la KB veniva popolata e indicizzata ma mai consumata dalla chat
# (il task type dominante e' `agentic_default`). Punto unico: questo set governa
# entrambi i gate (`_build_rag_context`, `_build_kb_rag_context`).
_RAG_INTENTS = {"code", "code_edit", "code_read", "refactor", "analyze",
                "fix", "implement", "debug", "review",
                "agentic_default", "system_admin"}

# Soglia minima di similarita' per includere un'interazione nel contesto.
# Sotto questa soglia il match non e' significativo e introdurrebbe rumore.
_RAG_MIN_SCORE = 0.5


def _rag_top_k() -> int:
    """Numero massimo di interazioni recuperate per turno (DB-driven, mig 0217).

    Default alzato a 12 (era hardcoded 5): con l'offload lossless il RAG e' la
    fonte di verita' del contenuto troncato, quindi il recupero non deve essere
    artificialmente stretto. Fallback safe se DB down.
    """
    from .. import context_offload

    return int(context_offload._load_offload_config()["rag_top_k"])


def _rag_snippet_max_chars() -> int:
    """Limite caratteri per snippet RAG incluso (DB-driven, mig 0217).

    Default alzato a 4000 (era hardcoded 400): snippet piu' ampi riducono la
    necessita' di round-trip e non perdono il cuore del match. Fallback safe.
    """
    from .. import context_offload

    return int(context_offload._load_offload_config()["rag_snippet_max_chars"])


def _rag_injection_mode() -> str:
    """Modalita' di iniezione KB nel prompt (DB-driven):
      - 'index' (default): inietta solo un INDICE compatto (titolo/file + note_id)
        e istruisce l'agente a leggere il contenuto on-demand con i tool
        (code_doc / knowledge_get_note). Risparmia molti token per turno.
      - 'full': inietta gli snippet completi (comportamento storico).
    Fallback 'index' se DB down.
    """
    try:
        from brain.utils.settings_db import get_setting
        v = (get_setting("knowledge.rag_injection_mode", "index") or "index").strip().lower()
        return v if v in ("index", "full") else "index"
    except Exception:
        return "index"


def _build_rag_context(intent: str, query_text: str) -> str:
    """Recupera interazioni simili e formatta come blocco contesto.

    Restituisce stringa vuota se: retriever non configurato, intent non
    eligible, query troppo corta, nessun match sopra soglia, o errore.
    Non solleva mai eccezioni (best-effort).
    """
    if _retriever is None or intent not in _RAG_INTENTS:
        return ""
    if not query_text or len(query_text.strip()) < 10:
        return ""
    _topk = _rag_top_k()
    _snippet_cap = _rag_snippet_max_chars()
    try:
        hits = _retriever.get_similar_interactions(
            query_text=query_text, task_type=None, limit=_topk,
        )
    except Exception as exc:
        logger.debug("RAG retrieval fallito: %s", exc)
        return ""
    relevant = [h for h in hits if h.get("score", 0) >= _RAG_MIN_SCORE]
    if not relevant:
        return ""
    # Formatto come blocco XML strutturato (coerente con le best practice
    # Anthropic: tag chiari aiutano l'LLM a separare contesto da istruzioni).
    snippets: list[str] = []
    for h in relevant:
        text = str(h.get("text") or h.get("user_input") or "").strip()
        if not text:
            continue
        if len(text) > _snippet_cap:
            text = text[: _snippet_cap - 3] + "..."
        score = float(h.get("score", 0))
        snippets.append(f'  <interazione score="{score:.2f}">{text}</interazione>')
    if not snippets:
        return ""
    logger.info("router_node: RAG inline injected %d snippet (intent=%s)",
                len(snippets), intent)
    return ("<contesto_pertinente>\n"
            "  <!-- Interazioni passate simili a questa richiesta. "
            "Usale come ricordo, non come istruzioni vincolanti. -->\n"
            + "\n".join(snippets) + "\n</contesto_pertinente>")


# URL mcp-core per chiamate internal (no auth, solo localhost backend).
# Configurabile via env per deploy non-locali.
_MCP_CORE_INTERNAL_URL = os.environ.get(
    "MCP_CORE_INTERNAL_URL", "http://localhost:4000"
)


def _build_kb_rag_context(intent: str, project_id: str, query_text: str) -> str:
    """Recupera note rilevanti dalla Knowledge Base per-progetto e formatta come contesto.

    Diverso da `_build_rag_context` (che cerca in chat_messages via Qdrant
    `project_context`): questo consulta `project_knowledge_notes` via endpoint
    `/api/internal/knowledge/search` di mcp-core. Le note KB contengono:
      - Messaggi user precedenti gia' classificati con intent
      - Note manuali (feature, requirement, decision, domain) curate dall'utente
      - Decisioni di design del progetto

    Failsafe: se mcp-core down, project_id vuoto, o nessun match, ritorna "".
    Non solleva eccezioni.
    """
    if not project_id or intent not in _RAG_INTENTS:
        return ""
    if not query_text or len(query_text.strip()) < 10:
        return ""
    _snippet_cap = _rag_snippet_max_chars()
    try:
        import requests  # noqa: PLC0415 (import lazy)
        resp = requests.post(
            f"{_MCP_CORE_INTERNAL_URL}/api/internal/knowledge/search",
            json={
                "project_id": project_id,
                "query": query_text,
                "top_k": _rag_top_k(),
                "min_score": _RAG_MIN_SCORE,
            },
            timeout=5,
        )
        if resp.status_code != 200:
            logger.debug("KB RAG: HTTP %s da mcp-core", resp.status_code)
            return ""
        data = resp.json()
    except Exception as exc:
        logger.debug("KB RAG retrieval fallito: %s", exc)
        return ""

    results = data.get("results", [])
    if not results:
        return ""

    mode = _rag_injection_mode()
    snippets: list[str] = []
    for r in results:
        title = str(r.get("title") or "").strip()
        snippet_text = str(r.get("snippet") or "").strip()
        if not title and not snippet_text:
            continue
        intent_attr = r.get("intent") or "chat"
        kind_attr = str(r.get("kind") or "")
        note_id = str(r.get("note_id") or "")
        score = float(r.get("score") or 0)

        if mode == "index":
            # Solo indice compatto: l'agente legge il contenuto on-demand con i
            # tool (code_doc / knowledge_get_note). Prompt molto piu' leggero.
            if kind_attr == "code_doc":
                snippets.append(f'  <doc_codice file="{title}" score="{score:.2f}"/>')
            else:
                snippets.append(
                    f'  <nota intent="{intent_attr}" titolo="{title}" '
                    f'note_id="{note_id}" score="{score:.2f}"/>'
                )
            continue

        # mode == "full": snippet completo (comportamento storico).
        if len(snippet_text) > _snippet_cap:
            snippet_text = snippet_text[: _snippet_cap - 3] + "..."
        if kind_attr == "code_doc":
            snippets.append(
                f'  <doc_codice file="{title}" score="{score:.2f}">\n'
                f'    <contenuto>{snippet_text}</contenuto>\n'
                f'  </doc_codice>'
            )
        else:
            snippets.append(
                f'  <nota intent="{intent_attr}" score="{score:.2f}">\n'
                f'    <titolo>{title}</titolo>\n'
                f'    <contenuto>{snippet_text}</contenuto>\n'
                f'  </nota>'
            )
    if not snippets:
        return ""

    logger.info("router_node: KB-RAG injected %d note (mode=%s, intent=%s, project=%s)",
                len(snippets), mode, intent, project_id[:8])
    if mode == "index":
        guida = (
            "  <!-- INDICE della Knowledge Base rilevante (titoli/file, non il\n"
            "       contenuto). Per leggere il dettaglio usa i TOOL on-demand:\n"
            "       code_doc(file_path) per la doc di un file (cosa fa, dipendenze,\n"
            "       call-graph) PRIMA di modificarlo; knowledge_get_note(note_id)\n"
            "       per il testo completo di una nota. <doc_codice> = file gia'\n"
            "       documentato: NON re-implementarlo da zero, evita ripetizioni\n"
            "       ed errori gia' risolti. -->\n"
        )
    else:
        guida = (
            "  <!-- Note dal Knowledge Base del progetto: contesto, decisioni,\n"
            "       requirement, messaggi gia' affrontati, e <doc_codice> ossia la\n"
            "       documentazione (code-wiki) dei file esistenti. Usa per evitare\n"
            "       duplicazioni/ripetizioni, riusare il codice esistente e non\n"
            "       reintrodurre errori gia' risolti. La doc_codice descrive cosa\n"
            "       fa gia' un file: NON re-implementarlo da zero. -->\n"
        )
    return "<knowledge_base_progetto>\n" + guida + "\n".join(snippets) + "\n</knowledge_base_progetto>"


# ─── Nodo: router ────────────────────────────────────────────────────────────

# Meta-tool di gestione/discovery Nexus mantenuti per gli intent conversazionali.
# Punto unico (regola L): un solo posto definisce cosa resta disponibile a una
# chat. NON include tool con side-effect (write_file/edit_file/run_command/...):
# una chat puo' ISPEZIONARE il progetto (scoprendo read_file/list_files via
# nexus_mcp_tool_search e invocandoli via nexus_mcp_tool_call) ma non modificarlo.
_CHAT_DISCOVERY_KEEP = frozenset({
    "nexus_mcp_tool_search",
    "nexus_mcp_tool_call",
    "nexus_open_file_in_editor",
    "recall_context",
})


def filter_chat_discovery_tools(tools_json: list[dict]) -> list[dict]:
    """Per gli intent conversazionali tiene solo i meta-tool di gestione/discovery.

    Prima qui si azzerava tools_json (chat single-shot senza tool): una domanda
    riferita al progetto ma formulata in modo informativo ("perche' ci sono due
    index.html?") riceveva una risposta generica su progetti ipotetici, perche'
    il modello non poteva ispezionare i file reali. Mantenendo i meta-tool il
    modello puo' scoprire e usare gli strumenti di lettura se la domanda lo
    richiede, restando comunque rapido sullo small-talk (nessuna tool call).
    """
    return [t for t in (tools_json or []) if t.get("name") in _CHAT_DISCOVERY_KEEP]


async def router_node(state: AgentState) -> dict[str, Any]:
    """Classifica l'intent utente e prepara il routing del modello.

    Passi:
    1. Classifica l'intent (semantico).
    2. Se `profile_name` e' esplicito, lo usa senza consultare Q-learning.
    3. Altrimenti: se e' configurato `AgentRouterClient`, invoca
       `select_agent(task_type=intent, instructions=testo)` e usa il
       profilo suggerito. Fallback al mapping statico intent→profilo.
    4. Inietta `system_text` dal `prompt_registry` e filtra `tools_json`.
    """
    messages = state.get("messages", [])
    if not messages:
        return {
            "user_intent": "chat",
            "task_type": "chat",
            "behavior_mode": state.get("behavior_mode", "bilanciata"),
            "token_budget": 400,
            "iterations": state.get("iterations", 0) + 1,
        }

    last_message = messages[-1]
    text = last_message.content if hasattr(last_message, "content") else str(last_message)

    # Stima token budget (approssimazione: 1 token ~ 4 caratteri)
    token_budget = max(400, len(text) // 4)

    # ── Classificazione intent: SOLO semantica via LLM ───────────────────────
    # Niente piu' keyword/embedding ne' fast-path/override basati sul confronto
    # di stringhe. L'AgenticIntentClassifier (LLM, cache TTL 24h) e' l'UNICO
    # interprete; su LLM down ritorna l'intent neutro `agentic_default` (che
    # attiva il _LAZY_MINIMAL_TOOLKIT cosi' l'agente interpreta e agisce da se').
    # I metadati complexity/agentic_score/is_ambiguous alimentano il gating del
    # planner (route_after_router).
    intent = "agentic_default"
    intent_confidence = 0.5
    task_complexity: str | None = None
    agentic_score_val: float | None = None
    is_ambiguous_val: bool | None = None
    if _agentic_classifier is not None:
        try:
            ag = await _agentic_classifier.classify(str(text))
            if ag is not None:
                intent = getattr(ag, "intent", None) or "agentic_default"
                _ag_conf = getattr(ag, "confidence", None)
                if _ag_conf is not None:
                    intent_confidence = float(_ag_conf)
                task_complexity = getattr(ag, "complexity", None)
                agentic_score_val = getattr(ag, "agentic_score", None)
                is_ambiguous_val = getattr(ag, "is_ambiguous", None)
                logger.info(
                    "router_node: classifier LLM -> intent=%s conf=%.2f complexity=%s "
                    "agentic=%.2f ambiguous=%s",
                    intent, intent_confidence, task_complexity,
                    agentic_score_val or 0.0, is_ambiguous_val,
                )
        except Exception as _ag_exc:  # noqa: BLE001
            logger.warning(
                "router_node: classifier LLM fallito (%s) -> agentic_default", _ag_exc
            )
            intent = "agentic_default"
            intent_confidence = 0.5
    else:
        logger.warning(
            "router_node: _agentic_classifier non configurato -> agentic_default"
        )

    behavior_mode = state.get("behavior_mode", "bilanciata")

    # Boost token_budget per intent complessi che richiedono piu' output
    # (generazione documenti, analisi codice, fix estesi). Inclusi anche gli
    # intent "attivi" che usano tool e producono output reale (file_ops,
    # architecture, system_admin): senza boost restavano al floor di 400 token,
    # insufficiente per un turno agentico con tool_use + risposta.
    COMPLEX_INTENTS = {
        "doc_generate", "analyze", "fix", "refactor", "implement",
        "file_ops", "architecture", "system_admin",
    }
    # Boost basato solo sull'intent semantico (niente piu' keyword sul testo).
    # agentic_default (fallback LLM-down) e' agentico: merita il budget pieno.
    if intent in COMPLEX_INTENTS or intent == "agentic_default":
        token_budget = max(token_budget, 4096)

    # ── Profile selection ────────────────────────────────────────────────
    # Se il chiamante non ha forzato un profile_name, lo deriviamo dall'intent.
    # Il profile decide: system_text (da prompt_registry), allowed_tools.
    explicit_profile = state.get("profile_name")
    profile = None
    if explicit_profile:
        profile = profile_loader.get_profile(explicit_profile)
        if profile is None:
            logger.warning("router_node: profile_name='%s' sconosciuto, ignoro",
                           explicit_profile)
    if profile is None and _agent_router is not None:
        # Consultiamo il Q-Learning router (sub-nodo): best-effort, se il
        # canale gRPC non risponde ignoriamo silenziosamente.
        try:
            last_text = str(text)[:4096]
            sel = await _agent_router.select_agent(
                task_type=intent, instructions=last_text,
                task_id=state.get("thread_id"),
            )
            # Gating: ci si fida del Q-router SOLO quando la scelta deriva da
            # Q-value appresi (EXPLOITATION) o e' forzata (FORCED). In COLD_START
            # (Q-table ancora vuota) ed EXPLORATION (epsilon-greedy random) la
            # selezione e' basata su similarity/caso e produce profili incoerenti
            # col task (es. 'tester' per file_ops): in quei casi si preferisce il
            # fallback statico intent->profilo (route_profile_for_intent piu' sotto).
            _trusted = sel.strategy in ("EXPLOITATION", "FORCED")
            if not sel.is_empty and _trusted:
                suggested = profile_loader.get_profile(sel.agent_type)
                if suggested is not None:
                    logger.info(
                        "router_node: agent_router suggerisce profile=%s "
                        "strategy=%s q_value=%.3f conf=%.3f",
                        suggested.name, sel.strategy, sel.q_value, sel.confidence,
                    )
                    profile = suggested
                else:
                    logger.debug(
                        "router_node: agent_router propone profile '%s' "
                        "non presente nel catalog",
                        sel.agent_type,
                    )
            elif not sel.is_empty:
                logger.info(
                    "router_node: agent_router strategy=%s (q=%.3f) non affidabile, "
                    "uso fallback statico intent->profilo",
                    sel.strategy, sel.q_value,
                )
        except Exception as exc:
            logger.debug("router_node: agent_router select fallito: %s", exc)

    if profile is None:
        profile = profile_loader.route_profile_for_intent(intent)

    # Mig 0181 — Calcola budget iter adattivo se non già impostato (primo turno).
    # Prompt complesso (fullstack scaffolding, refactor end-to-end) -> budget alto.
    # Prompt semplice (chat, fix mirato) -> budget basso. Sostituisce il
    # MAX_AGENT_ITERATIONS costante in route_after_executor.
    iter_budget_existing = int(state.get("iteration_budget") or 0)
    if iter_budget_existing <= 0:
        # Primo messaggio del turno: prende il content dell'ULTIMO HumanMessage
        # (quello che ha appena innescato il run, il piu' rappresentativo del task).
        last_human_text = ""
        for m in reversed(messages):
            if hasattr(m, "type") and m.type == "human":
                content = getattr(m, "content", "")
                if isinstance(content, list):
                    content = " ".join(
                        b.get("text", "") for b in content if isinstance(b, dict)
                    )
                last_human_text = str(content)
                break
        initial_model = state.get("model_override") or state.get("sticky_model")
        iter_budget, complexity_score = compute_iteration_budget(last_human_text, initial_model)
        logger.info(
            "router_node: adaptive budget iter=%d complexity=%d model=%s prompt_len=%d",
            iter_budget, complexity_score, initial_model or "?", len(last_human_text)
        )
    else:
        # Iterazioni successive: mantiene il budget calcolato al primo turno.
        iter_budget = iter_budget_existing
        complexity_score = int(state.get("complexity_score") or 0)

    updates: dict[str, Any] = {
        "user_intent": intent,
        "task_type": intent,
        "behavior_mode": behavior_mode,
        "token_budget": token_budget,
        "iterations": state.get("iterations", 0) + 1,
        "intent_confidence": intent_confidence,
        "iteration_budget": iter_budget,
        "complexity_score": complexity_score,
    }
    # PR-D: segnali del classifier agentico per il gating adattivo (se prodotti).
    if task_complexity is not None:
        updates["task_complexity"] = task_complexity
    if agentic_score_val is not None:
        updates["agentic_score"] = agentic_score_val
    if is_ambiguous_val is not None:
        updates["is_ambiguous"] = is_ambiguous_val

    if profile is not None:
        updates["profile_name"] = profile.name
        # Se il chiamante non ha gia' passato un system_text, lo prendiamo
        # dal registry tramite la prompt_key del profilo. Non sovrascriviamo
        # un system_text esplicito per preservare override utente.
        if not state.get("system_text"):
            resolved = prompt_registry.get_prompt(profile.prompt_key)
            if resolved:
                # Sostituisce placeholder runtime ({{lang_hint}}, {{type_hint}},
                # {{repo_summary}}). Mai lasciare {{...}} letterale al provider.
                rendered = prompt_renderer.render(resolved, dict(state), intent)
                # ── RAG inline (BP7 piano riduzione token) ─────────────────
                # Per intent code-related, recupera interazioni simili da Qdrant
                # e prependi come contesto pertinente. Riduce le tool call
                # ridondanti (read_file dello stesso file) su task ricorrenti.
                rag_block = _build_rag_context(intent, str(text))
                if rag_block:
                    rendered = rag_block + "\n\n" + rendered
                # KB inline: cerca note rilevanti nella Knowledge Base del progetto.
                # Diverso da _build_rag_context (interazioni chat passate):
                # qui prendiamo note auto+manuali con context specifico del progetto.
                kb_block = _build_kb_rag_context(intent, str(state.get("project_id") or ""), str(text))
                if kb_block:
                    rendered = kb_block + "\n\n" + rendered
                # PR-3 Codex pattern: project_instructions.md injection nel system_text.
                # PR-3 Cursor pattern: <available_subagents> block per auto-delegation.
                try:
                    extra_blocks = _build_pr3_system_blocks(state)
                    if extra_blocks:
                        rendered = rendered + "\n\n" + extra_blocks
                except Exception as _exc:
                    logger.debug("pr3 injection skip: %s", _exc)
                updates["system_text"] = rendered
        # Task-Playbook Engine (mig 0366): inietta una guida di dominio riusabile
        # (es. "implementa da Figma Make") quando il contesto del turno matcha un
        # playbook in nexus_task_playbooks. La conoscenza di "come si fa" vive in
        # DB, non nel prompt utente. Punto unico, attivo anche in automatico.
        # FUORI dal blocco "if not system_text": il chiamante (mcp-core) spesso
        # passa gia' un system_text, quindi va appeso al testo EFFETTIVO. Guardia
        # anti-duplicato per non riaccodarlo ai turni successivi. Best-effort.
        try:
            from .. import task_playbook
            effective_st = updates.get("system_text") or state.get("system_text") or ""
            if effective_st and "<task_playbook" not in effective_st:
                pb_block = task_playbook.guidance_for({"intent": intent, "text": str(text)})
                if pb_block:
                    updates["system_text"] = effective_st + "\n\n" + pb_block
        except Exception as _exc:
            logger.debug("task_playbook injection skip: %s", _exc)
        # Filtriamo tools_json combinando whitelist profilo + intent (BP5).
        # Il doppio filtraggio: per chat/analyze/review viene rimosso anche
        # cio' che il profilo permetterebbe ma che non serve all'intent.
        tools = state.get("tools_json") or []
        if tools:
            filtered = profile.filter_tools_for_intent(tools, intent)
            if len(filtered) != len(tools):
                logger.info(
                    "router_node: profile=%s intent=%s filtra tools %d -> %d",
                    profile.name, intent, len(tools), len(filtered),
                )
                updates["tools_json"] = filtered
        logger.info(
            "router_node: intent=%s token_budget=%d mode=%s profile=%s",
            intent, token_budget, behavior_mode, profile.name,
        )
    else:
        # Nessun profilo trovato: applica comunque _INTENT_TOOL_SUBSET (BP5 fallback).
        # Senza profilo il filtro combinato profilo×intent non scatta, ma possiamo
        # applicare il subset intent-only direttamente: code_read riceve 6 tool
        # di sola lettura invece di tutti i 31 tool builtin. Riduce il payload
        # di ogni turno del loop del 70-80%.
        tools = state.get("tools_json") or []
        if tools and intent:
            intent_subset = profile_loader._INTENT_TOOL_SUBSET.get(intent)
            if intent_subset is not None and intent_subset != ["*"]:
                _always_on = profile_loader.AgentProfile._ALWAYS_ON_TOOLS
                allow = set(intent_subset) | _always_on
                filtered_np = [t for t in tools if t.get("name") in allow]
                if len(filtered_np) != len(tools):
                    logger.info(
                        "router_node: intent=%s filtra tools %d -> %d (no-profile fallback)",
                        intent, len(tools), len(filtered_np),
                    )
                    updates["tools_json"] = filtered_np
        logger.info(
            "router_node: intent=%s token_budget=%d mode=%s profile=<none>",
            intent, token_budget, behavior_mode,
        )

    # ── Chat: meta-tool di gestione/discovery, NON azzeramento totale ───────
    # Prima azzeravamo tools_json per gli intent conversazionali, forzando la
    # completion single-shot. Effetto collaterale grave: una domanda riferita al
    # progetto ma formulata in modo informativo ("perche' ci sono due
    # index.html?", "le form sono mal disposte") che il classifier etichetta
    # "chat" riceveva una risposta GENERICA su progetti ipotetici, perche' il
    # modello non aveva alcun tool per ispezionare i file reali.
    #
    # Ora manteniamo SOLO i meta-tool di gestione tool di Nexus
    # (nexus_mcp_tool_search + nexus_mcp_tool_call) piu' open-file e
    # recall_context. Questo lascia tools_json NON vuoto -> ramo agentico, ma:
    #   - per small-talk vero ("ciao", "grazie") il modello non chiama alcun
    #     tool e risponde diretto (nessun overhead di scrittura/esecuzione);
    #   - per una domanda sul progetto il modello scopre i tool di lettura
    #     (read_file/list_files/...) via nexus_mcp_tool_search e li invoca via
    #     nexus_mcp_tool_call, ispezionando il progetto REALE invece di
    #     rispondere a vuoto.
    # Restano esclusi i tool con side-effect (write_file/edit_file/run_command/
    # delete_file/...): una chat non deve modificare il progetto; se serve una
    # modifica la richiesta viene classificata con un altro intent (fix/code_*).
    if intent in ("chat", "general_chat"):
        _chat_base_tools = updates.get("tools_json", state.get("tools_json") or [])
        _chat_kept = filter_chat_discovery_tools(_chat_base_tools)
        updates["tools_json"] = _chat_kept
        logger.info(
            "router_node: intent=%s -> meta-tool gestione/discovery (%d tool): "
            "il modello puo' scoprire e ispezionare il progetto se la domanda lo richiede",
            intent, len(_chat_kept),
        )

    # ── Meta-step `routing` per pubblicazione in chat ───────────────────────
    # Emesso solo se la classificazione e' significativa (no chat banale a 0
    # confidence senza profilo) e se il flag `meta_steps.routing_enabled`
    # e' attivo. Riduce rumore nelle conversazioni di small-talk.
    if profile is not None or intent != "chat":
        profile_name = profile.name if profile is not None else None
        title = f"Intent: {intent}"
        if profile_name:
            title += f" — profilo {profile_name}"
        # Provider+model noti al routing-time (potranno cambiare in cascade,
        # ma e' il valore deciso dalla routing matrix per QUESTO turno).
        # Risolvi provider via _provider_from_model (ai_price_catalog, cache).
        _routing_model = (
            state.get("model_override")
            or state.get("sticky_model")
            or locals().get("initial_model")
        )
        _routing_provider = (
            state.get("provider_override")
            or state.get("sticky_provider")
            or (_provider_from_model(_routing_model) if _routing_model else None)
        )
        routing_meta = meta_steps.make(
            kind="routing",
            title=title,
            payload={
                "intent": intent,
                "task_type": intent,
                "profile_name": profile_name,
                "behavior_mode": behavior_mode,
                "token_budget": token_budget,
                # Per-turn provider/model — visualizzati nel badge UI con
                # colore brand-specific + tonalita' basata sul costo.
                "provider": _routing_provider,
                "model": _routing_model,
            },
        )
        if routing_meta:
            updates["meta_steps"] = [routing_meta]
            meta_steps.persist_async(state.get("thread_id"), routing_meta)

    # ── Nexus thinking (visibilita' processo decisionale) ──────────────────
    try:
        _profile_name = profile.name if profile is not None else None
        _thinking_lines: list[str] = []
        _thinking_lines.append(
            f"Classificato intent '{intent}' (confidence {intent_confidence:.2f})."
        )
        _thinking_lines.append(
            f"Modalita' comportamento: {behavior_mode}, token budget stimato {token_budget}."
        )
        if complexity_score:
            _thinking_lines.append(
                f"Complessita' stimata del task: {complexity_score} punti, budget iterazioni {iter_budget}."
            )
        if _profile_name:
            _thinking_lines.append(f"Profilo agente selezionato: {_profile_name}.")
        _emit_thinking(updates, *_thinking_lines)
    except Exception as _thinking_exc:
        logger.debug("router_node: thinking emit fallito: %s", _thinking_exc)

    return updates


# ─── Nodo: executor ──────────────────────────────────────────────────────────


def _compress_marker(content: str) -> str:
    """Offload LOSSLESS del contenuto completo + marker da appendere al placeholder.

    Chiamato da _compress_old_tool_results PRIMA di sostituire un tool_result
    vecchio con la sua versione compressa. Indicizza il contenuto intero in RAG
    (idempotente per hash) cosi' il dato resta recuperabile via
    `nexus_search_semantic` anche dopo la compressione nel prompt.

    Best-effort: se l'offload non e' disponibile, il marker segnala comunque la
    compressione (degraded). Non solleva mai.
    """
    from .. import context_offload

    offload = context_offload.offload_to_rag(
        _embeddings, content, source_kind="tool_result_compressed",
    )
    if offload is not None:
        return (
            f"\n[... compresso: {len(content)} char originali INDICIZZATI in RAG "
            f"(ref={offload['ref'][:12]}). Recupera con nexus_search_semantic ...]"
        )
    return f"\n[... compresso: {len(content)} char originali ...]"


def _compress_old_tool_results(
    messages: list[Any],
    keep_recent: int = 6,
    max_content_chars: int = 500,
) -> list[Any]:
    """Comprime i tool_result dei messaggi piu' vecchi per ridurre il contesto.

    Mantiene intatti gli ultimi `keep_recent` messaggi. Per i precedenti,
    i blocchi tool_result con contenuto > `max_content_chars` char vengono
    sostituiti con un riassunto troncato.

    `max_content_chars` e' configurabile: nelle fasi di loop avanzato
    (iterazioni alte) l'executor passa un valore piu' basso (es. 150)
    per una compressione piu' aggressiva.

    Prima della compressione applica _dedup_tool_results: spesso la stessa
    risorsa viene letta piu' volte e il dedup risparmia tokens senza
    perdere informazione (l'ultima copia e' preservata).
    """
    # Step 0: dedup duplicati su tutta la history (anche keep_recent benefici).
    messages = _dedup_tool_results(messages)
    if len(messages) <= keep_recent:
        return messages

    compressed = []
    boundary = len(messages) - keep_recent
    # La finestra "recente" usa una soglia piu' permissiva: 2× max_content_chars.
    recent_threshold = max_content_chars * 2

    for i, m in enumerate(messages):
        if i >= boundary:
            # Anche i messaggi recenti vengono compressi, ma con soglia piu' alta.
            extra = getattr(m, "additional_kwargs", {}) or {}
            blocks = extra.get("anthropic_content")
            if blocks is None or not isinstance(blocks, list):
                compressed.append(m)
                continue
            changed = False
            new_blocks = []
            for block in blocks:
                if not isinstance(block, dict) or block.get("type") != "tool_result":
                    new_blocks.append(block)
                    continue
                content = block.get("content", "")
                if isinstance(content, str) and len(content) > recent_threshold:
                    kept = max(recent_threshold // 2, 200)
                    new_blocks.append({
                        **block,
                        "content": content[:kept] + _compress_marker(content),
                    })
                    changed = True
                else:
                    new_blocks.append(block)
            if changed:
                new_msg = HumanMessage(
                    content=getattr(m, "content", ""),
                    additional_kwargs={"anthropic_content": new_blocks},
                )
                compressed.append(new_msg)
            else:
                compressed.append(m)
            continue

        extra = getattr(m, "additional_kwargs", {}) or {}
        blocks = extra.get("anthropic_content")
        if blocks is None or not isinstance(blocks, list):
            compressed.append(m)
            continue

        changed = False
        new_blocks = []
        for block in blocks:
            if not isinstance(block, dict) or block.get("type") != "tool_result":
                new_blocks.append(block)
                continue
            content = block.get("content", "")
            if isinstance(content, str) and len(content) > max_content_chars:
                kept = max(max_content_chars // 2, 100)
                new_blocks.append({
                    **block,
                    "content": content[:kept] + _compress_marker(content),
                })
                changed = True
            else:
                new_blocks.append(block)

        if changed:
            new_msg = HumanMessage(
                content=getattr(m, "content", ""),
                additional_kwargs={"anthropic_content": new_blocks},
            )
            compressed.append(new_msg)
        else:
            compressed.append(m)

    return compressed


_AGGRESSIVE_TRUNC_MARKER = "[...troncato per limite contesto...]"


def _truncate_message_content(m: Any, max_content_chars: int) -> tuple[Any, bool]:
    """Tronca AGGRESSIVAMENTE il contenuto di un messaggio (incluso assistant).

    A differenza di `_compress_old_tool_results` (che tocca solo i blocchi
    tool_result), qui si troncano TUTTI i blocchi testuali lunghi:
    - blocchi `text` (ragionamenti assistant),
    - blocchi `tool_result` (output tool),
    - input dei blocchi `tool_use` se l'arg testuale e' enorme,
    - `content` stringa diretto.

    I blocchi `tool_use` mantengono `id`/`name` intatti (necessari per il
    pairing con i tool_result Anthropic): si tronca solo il payload `input`.

    Ritorna (nuovo_messaggio, changed). Non solleva mai.
    """
    changed = False
    extra = getattr(m, "additional_kwargs", {}) or {}
    blocks = extra.get("anthropic_content")

    if blocks is not None and isinstance(blocks, list):
        new_blocks: list[Any] = []
        for block in blocks:
            if not isinstance(block, dict):
                new_blocks.append(block)
                continue
            btype = block.get("type")
            if btype in ("text", "tool_result"):
                # I blocchi text Anthropic portano il testo in "text"; i
                # tool_result in "content". Alcuni text usano "content".
                content_key = "content"
                if btype == "text":
                    content = block.get("text")
                    if isinstance(content, str):
                        content_key = "text"
                    else:
                        content = block.get("content", "")
                else:
                    content = block.get("content", "")
                if isinstance(content, str) and len(content) > max_content_chars:
                    kept = max(max_content_chars - len(_AGGRESSIVE_TRUNC_MARKER), 50)
                    truncated = content[:kept] + _compress_marker(content) + _AGGRESSIVE_TRUNC_MARKER
                    nb = {**block, content_key: truncated}
                    new_blocks.append(nb)
                    changed = True
                    continue
                new_blocks.append(block)
            elif btype == "tool_use":
                tin = block.get("input")
                try:
                    tin_str = json.dumps(tin, ensure_ascii=False, default=str)
                except Exception:
                    tin_str = str(tin)
                if len(tin_str) > max_content_chars:
                    nb = {**block, "input": {"_truncated": tin_str[:max_content_chars] + _AGGRESSIVE_TRUNC_MARKER}}
                    new_blocks.append(nb)
                    changed = True
                else:
                    new_blocks.append(block)
            else:
                new_blocks.append(block)
        if changed:
            cls = type(m)
            try:
                new_msg = cls(
                    content=getattr(m, "content", ""),
                    additional_kwargs={"anthropic_content": new_blocks},
                )
            except Exception:
                new_msg = HumanMessage(
                    content=getattr(m, "content", ""),
                    additional_kwargs={"anthropic_content": new_blocks},
                )
            return new_msg, True
        return m, False

    # content stringa diretto (assistant senza blocchi strutturati).
    content = getattr(m, "content", "")
    if isinstance(content, str) and len(content) > max_content_chars:
        kept = max(max_content_chars - len(_AGGRESSIVE_TRUNC_MARKER), 50)
        new_content = content[:kept] + _compress_marker(content) + _AGGRESSIVE_TRUNC_MARKER
        cls = type(m)
        try:
            new_msg = cls(content=new_content, additional_kwargs=extra)
        except Exception:
            return m, False
        return new_msg, True
    return m, False


def _compress_aggressive_token_based(
    messages: list[Any],
    keep_recent: int,
    max_content_chars: int,
) -> tuple[list[Any], bool]:
    """Compressione AGGRESSIVA: tronca TUTTI i messaggi vecchi (anche assistant).

    Preserva integri:
    - il PRIMO HumanMessage (richiesta originale),
    - gli eventuali messaggi 'summary' (riassunto rolling),
    - gli ultimi `keep_recent` messaggi.

    Tutto il resto viene troncato a `max_content_chars`. Ritorna
    (messaggi, changed). Non solleva mai (best-effort).
    """
    n = len(messages)
    if n <= keep_recent + 1:
        return messages, False
    first_human = _first_human_index(messages)
    boundary = n - keep_recent
    out: list[Any] = []
    any_changed = False
    for i, m in enumerate(messages):
        if i >= boundary or i == first_human or _is_summary_message(m):
            out.append(m)
            continue
        new_m, changed = _truncate_message_content(m, max_content_chars)
        out.append(new_m)
        any_changed = any_changed or changed
    return out, any_changed


def _apply_token_brake(
    messages: list[Any],
    model: str,
    cfg: dict[str, Any],
    iteration: int,
) -> list[Any]:
    """Freno TOKEN-based: se la stima token >= ratio*window, comprime aggressivo.

    ADR 0016 Fase D: usa il tokenizer reale (tiktoken cl100k_base) invece di
    `chars//4` che sottostimava fino a 3-5x su JSON tool_result, codice e CJK.
    Riusa `_estimate_context_tokens` (precisione ±2%) e `_model_context_window`.
    Ri-applica la compressione finche' la stima scende sotto la soglia o non c'e'
    piu' nulla da comprimere (max passate limitate). Se anche cosi' resta sopra
    il window, applica un cap di sicurezza hard (richiesta originale + ultimi 2).

    Logga solo CONTEGGI (mai payload/prompt/response). Non solleva mai.
    """
    try:
        window = _model_context_window(model)
        ratio = float(cfg.get("max_context_ratio", 0.70))
        keep_recent = int(cfg.get("aggressive_keep_recent", 3))
        max_chars = int(cfg.get("aggressive_max_chars", 200))
        threshold_tokens = int(window * ratio)

        est_tokens = _estimate_context_tokens(messages)
        if est_tokens < threshold_tokens:
            return messages

        tokens_before = est_tokens
        max_passes = 5
        for _ in range(max_passes):
            messages, changed = _compress_aggressive_token_based(
                messages, keep_recent=keep_recent, max_content_chars=max_chars
            )
            est_tokens = _estimate_context_tokens(messages)
            if est_tokens < threshold_tokens or not changed:
                break

        logger.info(
            "executor_node: freno TOKEN-based iter=%d window=%d soglia=%d "
            "token %d -> %d (ratio=%.2f keep_recent=%d max_chars=%d tokenizer=tiktoken)",
            iteration, window, threshold_tokens, tokens_before, est_tokens,
            ratio, keep_recent, max_chars,
        )

        # Cap di sicurezza hard: se ancora sopra il window pieno, riduci all'osso.
        if est_tokens >= window:
            first_human = _first_human_index(messages)
            keep_idx = set(range(max(0, len(messages) - 2), len(messages)))
            if first_human >= 0:
                keep_idx.add(first_human)
            messages = [m for i, m in enumerate(messages) if i in keep_idx]
            est_hard = _estimate_context_tokens(messages)
            logger.warning(
                "executor_node: context cap raggiunto, troncamento hard "
                "iter=%d window=%d token %d -> %d",
                iteration, window, est_tokens, est_hard,
            )
        return messages
    except Exception as exc:  # noqa: BLE001 - best-effort, mai bloccare il turno
        logger.warning("executor_node: freno TOKEN-based fallito: %s", exc)
        return messages


async def executor_node(state: AgentState) -> dict[str, Any]:
    """Chiama il provider LLM. In modalita' agent (tools_json non vuoto) usa
    `generate_agent_turn` e popola `pending_tool_uses` + `stop_reason`.
    Altrimenti usa il percorso legacy `generate_completion_async`.

    Raccoglie anche metriche estese: token (cache), costo, latency, temperatura.
    """
    import datetime

    messages = list(state.get("messages", []))
    intent = state.get("user_intent", "chat")
    behavior_mode = state.get("behavior_mode", "bilanciata")
    token_budget = state.get("token_budget", 400)
    tools_json = state.get("tools_json") or []
    system_text = state.get("system_text") or ""

    # ── M16: merge tool scoperti (discovery-first) per QUESTO turno ───────────
    # Il turno precedente ha eseguito nexus_mcp_tool_search; tool_dispatch_node
    # ha estratto i tool trovati in `discovered_tools_next_turn`. Qui li
    # iniettiamo come native nel tools_json (dedup per nome, cap DB-driven).
    # Durata 1 turno: il prossimo tool_dispatch riscrive [] se non c'e' search.
    _discovered = state.get("discovered_tools_next_turn") or []
    if _discovered and tools_json:
        try:
            from brain.utils.settings_db import get_int_setting
            _cap = get_int_setting("agent.tools.discovery_max_injected", 20)
        except Exception:
            _cap = 20
        _existing_names = {t.get("name") for t in tools_json if isinstance(t, dict)}
        _added = 0
        for _dt in _discovered:
            if _added >= _cap:
                break
            if isinstance(_dt, dict) and _dt.get("name") and _dt["name"] not in _existing_names:
                tools_json = tools_json + [_dt]
                _existing_names.add(_dt["name"])
                _added += 1
        if _added:
            logger.info("M16: iniettati %d tool scoperti come native -> tools=%d", _added, len(tools_json))

    # ── M16 strategia "2 puri + native forzato" ──────────────────────────────
    # Al turno di SCOPERTA (set esposto = solo i meta-tool di discovery, nessun
    # tool gia' scoperto, e siamo al primo turno agente) esponiamo SOLO
    # nexus_mcp_tool_search e rimuoviamo nexus_mcp_tool_call: cosi' il modello,
    # forzato da tool_choice (first_turn_force), e' costretto a CERCARE i tool che
    # gli servono invece di eseguire a vuoto o narrare. I tool trovati vengono
    # estratti da tool_dispatch_node e iniettati come native al turno successivo
    # (search->inject). Ai turni con native iniettati, o dopo il primo turno, si
    # lascia il set completo + tool_choice auto (il modello usa i native o chiude,
    # niente loop di chiusura).
    _DISCOVERY_META = {"nexus_mcp_tool_search", "nexus_mcp_tool_call"}
    if not _discovered and tools_json:
        _names = {t.get("name") for t in tools_json if isinstance(t, dict)}
        if _names and _names <= _DISCOVERY_META:
            try:
                from brain.providers._schema_utils import is_first_agent_turn
                _is_first = is_first_agent_turn(state.get("messages") or [])
            except Exception:
                _is_first = True
            if _is_first:
                tools_json = [t for t in tools_json if t.get("name") == "nexus_mcp_tool_search"]
                logger.info("M16: turno di scoperta -> espongo solo nexus_mcp_tool_search (forza search)")

    # ── Cluster 1: iniezione plan_rationale nel system_text ───────────────────
    # Se il planner ha prodotto un razionale (gated plan_rationale_enabled),
    # lo prependiamo al system_text dell'executor: cosi' chi esegue conosce il
    # "perche'" del piano, i vincoli e le alternative scartate (continuita'
    # semantica planner->executor). Vuoto/flag OFF => nessun effetto.
    if state.get("plan_rationale"):
        _rat = str(state.get("plan_rationale") or "").strip()
        _constraints = state.get("plan_constraints") or []
        _alternatives = state.get("plan_alternatives") or []
        _block = ["<piano_razionale>", _rat]
        if _constraints:
            _block.append("Vincoli/non-goal: " + "; ".join(str(c) for c in _constraints))
        if _alternatives:
            _alts = [
                f"{a.get('option','?')} (scartata: {a.get('rejected_because','?')})"
                for a in _alternatives if isinstance(a, dict)
            ]
            if _alts:
                _block.append("Alternative scartate: " + "; ".join(_alts))
        _block.append("</piano_razionale>")
        system_text = "\n".join(_block) + "\n\n" + system_text
        logger.info("executor_node: plan_rationale iniettato (%d char)", len(_rat))

    # ── PR-C: worker-mode (orchestrator-worker puro) ──────────────────────────
    # Solo nel run PRINCIPALE (subagent_depth 0/None): dentro un worker
    # (subagent_depth >= 1) NON si applica restrizione, altrimenti il worker
    # 'implement' non potrebbe scrivere. Quando attivo + plan_phase_active +
    # subagents_enabled, l'executor diventa un ORCHESTRATORE: usa il prompt
    # agent.orchestrator.base e filtra i tool sulla whitelist (read-only +
    # delega), forzando la delega ai worker economici.
    _subagent_depth = int(state.get("subagent_depth") or 0)
    if _subagent_depth == 0 and state.get("plan_phase_active"):
        try:
            # orchestrator_config e prompt_registry sono gia' importati a
            # module-level (riga 21-29). Un import locale qui dentro
            # trasformerebbe le variabili in `local` per tutta la funzione
            # `executor_node`, causando UnboundLocalError quando il flusso
            # NON entra in questo branch (es. sub-agent con _subagent_depth>=1)
            # ma poi usa orchestrator_config piu' avanti (vedi Comp.3b).
            if (
                orchestrator_config.worker_mode_enabled()
                and orchestrator_config.subagents_enabled()
            ):
                orch_prompt = prompt_registry.get_prompt("agent.orchestrator.base")
                if orch_prompt:
                    system_text = orch_prompt
                    whitelist = set(orchestrator_config.worker_mode_tool_whitelist())
                    if whitelist and tools_json:
                        filtered = [
                            t for t in tools_json
                            if (t.get("name") if isinstance(t, dict) else getattr(t, "name", None)) in whitelist
                        ]
                        # Non azzerare i tool: se il filtro svuota tutto (es.
                        # whitelist disallineata), lascia i tool originali per
                        # non bloccare il run.
                        if filtered:
                            tools_json = filtered
                    logger.info(
                        "executor_node: worker-mode attivo (orchestratore), tools=%d",
                        len(tools_json),
                    )
        except Exception as exc:
            logger.debug("executor_node: worker-mode skip (%s)", exc)

    # ── Comp.3b: DAG scheduler parallelo (opt-in, mutuamente esclusivo) ────────
    # Forma strutturata del worker-mode: se il piano ha dipendenze (depends_on)
    # e dag_parallel_enabled e' ON, esegue i todo "ready" in parallelo a ondate
    # via dispatch_subagents. Tutto il parallelismo e' confinato in QUESTA
    # invocazione del nodo (loop interno), niente fan-out LangGraph: lo state
    # resta serializzabile. Early-return: salta la chiamata LLM dell'executor.
    _dag_cfg = orchestrator_config.get()
    if (
        _subagent_depth == 0
        and state.get("plan_phase_active")
        and bool(_dag_cfg.get("dag_parallel_enabled"))
    ):
        try:
            from .. import dag_scheduler, todo_store as _ts
            _run_id = state.get("thread_id")
            _todos_now = _ts.list_todos(_run_id) if _run_id else []
            _has_deps = any(t.get("depends_on") for t in _todos_now)
            if _has_deps and dag_scheduler.compute_ready_layer(_todos_now):
                total_done = 0
                waves = 0
                # Safety cap sul numero di ondate: non puo' superare il numero
                # di todo (ogni ondata ne completa almeno uno, o si ferma).
                max_waves = len(_todos_now) + 1
                while waves < max_waves:
                    updates = await dag_scheduler.run_dag_layer(state, _tool_runner, _dag_cfg)
                    waves += 1
                    if not updates or not updates.get("active_todo_ids"):
                        break
                    total_done += int(updates.get("completed") or 0)
                    if int(updates.get("completed") or 0) == 0:
                        # Ondata senza completamenti (tutti falliti): stop per
                        # non ciclare a vuoto; i discendenti sono gia' skipped.
                        break
                logger.info(
                    "executor_node: DAG parallelo eseguito (%d ondate, %d todo completati, run_id=%s)",
                    waves, total_done, _run_id,
                )
                # end_turn: route_after_executor -> verifier (se ON, trova tutti
                # terminali e chiude) oppure -> learner. Niente chiamata LLM.
                return {
                    "stop_reason": "end_turn",
                    "pending_tool_uses": [],
                    "iterations": iterations,
                }
        except Exception as exc:
            logger.warning("executor_node: DAG scheduler skip (%s)", exc)

    # ── G1 cap: max re-execution per "risposta descrittiva su action request" ─
    # Se route_after_executor ci ha gia' re-mandato qui >= max_nudges volte
    # senza produrre tool call, fermiamo l'esecuzione con un messaggio
    # assistant esplicito (non si tenta neppure la chiamata al modello).
    # Questo evita loop infiniti quando il modello non rispetta il nudge G1
    # (anche perche' il nudge stesso puo' non essere iniettato, es. la
    # history contiene gia' tool call -> _has_tool_calls_in_history filtra).
    _g1_reroute_count = int(state.get("g1_reroute_count") or 0)
    _g1_max_nudges = _load_g1_max_nudges()
    # Rileva se questa entry e' una re-execution G1: il turno precedente
    # ha chiuso senza tool call (end_turn/stop/None) e abbiamo gia' almeno
    # un'iterazione alle spalle. Verifica anche action_request sul primo
    # messaggio umano per evitare di contare entry non-G1.
    _prev_stop_for_g1 = state.get("stop_reason")
    _prev_iterations_for_g1 = int(state.get("iterations") or 0)
    _is_g1_reentry = False
    if (
        _prev_iterations_for_g1 >= 1
        and _prev_stop_for_g1 in ("end_turn", "stop", None)
        and not (state.get("pending_tool_uses") or [])
    ):
        _first_human_for_g1 = next(
            (getattr(m, "content", "") for m in messages if hasattr(m, "type") and m.type == "human"),
            "",
        )
        if isinstance(_first_human_for_g1, list):
            _first_human_for_g1 = " ".join(
                b.get("text", "") for b in _first_human_for_g1 if isinstance(b, dict)
            )
        # Conta la re-entry G1 sia per richiesta action-oriented (input) sia
        # per intenzione annunciata e non compiuta (output): cosi' il cap
        # anti-loop si applica anche ai debug dove il primo messaggio non e'
        # imperativo ma il modello narra "Inizio verificando X" senza agire.
        _g1_should_count = (
            _detect_action_request(str(_first_human_for_g1))
            or _detect_unfulfilled_intent(_last_assistant_text(messages))
        )
        if _g1_should_count:
            # FIX G1 intelligente: se il modello sta legittimamente reagendo a
            # un tool_result d'errore recente (es. "npm install" fallito), NON
            # e' "descrittivo" ma "in difficolta'": dargli piu' tentativi prima
            # del cap. Bug osservato 30/05/2026: gemini-2.5-flash in loop su
            # rebrand shadcn-ui -> shadcn ha esaurito 3 reroute G1 in 7 iter,
            # nonostante stesse facendo 30+ tool call. Soluzione: contare il
            # reroute G1 SOLO se gli ultimi tool result non avevano errori, e
            # consentire una soft-extension (+50% max_nudges) se errori reali.
            _last_tool_was_error = _detect_recent_tool_error(messages, lookback=4)
            if _last_tool_was_error:
                logger.info(
                    "executor_node: re-entry G1 skip-count (tool_result d'errore recente, "
                    "il modello sta reagendo, non descrivendo) reroute_count=%d/%d",
                    _g1_reroute_count, _g1_max_nudges,
                )
            else:
                _is_g1_reentry = True
                _g1_reroute_count += 1
                logger.info(
                    "executor_node: re-entry G1 rilevata, reroute_count=%d/%d",
                    _g1_reroute_count, _g1_max_nudges,
                )
    if _g1_reroute_count >= _g1_max_nudges:
        # Escalation orchestratore: invece di arrenderci e scaricare il problema
        # sull'utente ("usa un modello piu' capace"), e' l'orchestratore stesso
        # a promuovere il turno a un modello migliore (catena DB), azzerando il
        # contatore reroute cosi' il nuovo modello ha il suo budget di tentativi.
        # Solo a catena di escalation ESAURITA ci fermiamo davvero.
        _g1_cur_provider = (
            state.get("provider_used")
            or state.get("sticky_provider")
            or state.get("provider_override")
        )
        _g1_cur_model = (
            state.get("model_used")
            or state.get("sticky_model")
            or state.get("model_override")
        )
        _g1_escal = int(state.get("auto_escalations") or 0)
        _g1_picked = (
            _pick_escalation_model(_g1_cur_provider, _g1_cur_model, _g1_escal)
            if _g1_escal < 3
            else None
        )
        if _g1_picked:
            _esc_provider, _esc_model = _g1_picked
            logger.warning(
                "executor_node: G1 cap (%d) su %s/%s -> ESCALATION orchestratore a "
                "%s/%s (auto_escalations=%d), azzero reroute e ri-do il turno",
                _g1_max_nudges, _g1_cur_provider, _g1_cur_model,
                _esc_provider, _esc_model, _g1_escal + 1,
            )
            _esc_nudge = HumanMessage(
                content=(
                    f"Il modello precedente ha solo descritto le azioni senza "
                    f"eseguirle dopo {_g1_max_nudges} tentativi. Ora rispondi tu, "
                    f"che sei un modello piu' capace: NON descrivere, ESEGUI subito "
                    f"il prossimo step concreto con un tool call."
                )
            )
            return {
                "messages": [_esc_nudge],
                "sticky_provider": _esc_provider,
                "sticky_model": _esc_model,
                "auto_escalations": _g1_escal + 1,
                "g1_reroute_count": 0,
                "action_nudge_count": 0,
                "pending_tool_uses": [],
                "stop_reason": "g1_escalated",
                "iterations": int(state.get("iterations") or 0) + 1,
            }
        logger.warning(
            "executor_node: G1 cap raggiunto (reroute_count=%d >= max=%d) e catena "
            "escalation esaurita (auto_escalations=%d), interrompo esecuzione",
            _g1_reroute_count, _g1_max_nudges, _g1_escal,
        )
        _cap_text = (
            f"Modello non risponde con azione dopo {_g1_max_nudges} tentativi e "
            f"anche i modelli piu' capaci provati in escalation non hanno agito. "
            f"Fermo l'esecuzione: riformula la richiesta in modo piu' specifico."
        )
        return {
            "messages": [AIMessage(content=_cap_text)],
            "result": _cap_text,
            "pending_tool_uses": [],
            "stop_reason": "g1_cap_reached",
            "iterations": int(state.get("iterations") or 0) + 1,
            "g1_reroute_count": _g1_reroute_count,
            "action_nudge_count": int(state.get("action_nudge_count") or 0),
        }

    # ── Loop-detection semantica: esplorazione allegati senza scrittura ───────
    # Il contatore accumulato dai turni precedenti conta le chiamate a tool di
    # SOLA esplorazione (lettura/ispezione allegati e file). Se il modello varia
    # entry/offset la loop-detection per signature identica non scatta mai e si
    # arriva a 50+ iterazioni esplorative. Qui agiamo sulla FAMIGLIA di tool:
    #   - >= soglia: iniettiamo UN nudge forte verso la scrittura (una sola volta)
    #   - >= 2x soglia: abortiamo (il modello ha ignorato il nudge)
    # La soglia e' DB-driven (agent.exploration_loop_threshold, default 6).
    _exploration_count = int(state.get("consecutive_exploration_calls") or 0)
    _exploration_threshold = _load_exploration_loop_threshold()
    _exploration_nudge_sent = bool(state.get("exploration_nudge_sent") or False)
    _exploration_nudge_injected = False
    if _exploration_count >= 2 * _exploration_threshold:
        logger.warning(
            "executor_node: LOOP esplorativo (%d chiamate >= 2x soglia %d) senza "
            "scrittura. Abort.",
            _exploration_count, _exploration_threshold,
        )
        _expl_text = (
            f"[LOOP RILEVATO] Il modello ha eseguito {_exploration_count} "
            f"esplorazioni/ricerche-tool consecutive senza produrre un risultato "
            f"(ne' scrittura ne' risposta), ignorando il sollecito a procedere. "
            f"Esecuzione interrotta per evitare stallo. Riformula la richiesta in "
            f"modo piu' specifico o usa un modello piu' capace."
        )
        return {
            "messages": [AIMessage(content=_expl_text)],
            "result": _expl_text,
            "pending_tool_uses": [],
            "stop_reason": "loop_detected",
            "iterations": int(state.get("iterations") or 0) + 1,
            "consecutive_exploration_calls": _exploration_count,
            "exploration_nudge_sent": _exploration_nudge_sent,
        }
    if _exploration_count >= _exploration_threshold and not _exploration_nudge_sent:
        _expl_nudge = HumanMessage(
            content=(
                f"Hai gia' raccolto sufficiente contesto / cercato abbastanza "
                f"strumenti ({_exploration_count} esplorazioni). NON esplorare "
                f"oltre e NON cercare altri tool. Procedi ORA in base alla "
                f"richiesta: se devi MODIFICARE il progetto, scrivi i file con "
                f"write_file (e usa request_port per le porte); se invece era una "
                f"DOMANDA o una richiesta di proposte/opzioni, RISPONDI subito a "
                f"parole con il risultato, senza altre tool call."
            )
        )
        messages = list(messages) + [_expl_nudge]
        _exploration_nudge_sent = True
        _exploration_nudge_injected = True
        logger.info(
            "executor_node: nudge anti-esplorazione iniettato (count=%d, soglia=%d)",
            _exploration_count, _exploration_threshold,
        )

    # ── Loop-detection comando ripetuto con errori (Fix 30/05/2026) ──────────
    # Diverso da loop-detection esplorativa: qui rileviamo il pattern "stesso
    # comando di esecuzione (run_command/run_service) ripetuto N volte con
    # tool_result d'errore". Caso reale: gemini-2.5-flash su build npm/shadcn
    # ha chiamato `npm run build` 5 volte consecutive, ognuna fallita per
    # dipendenze mancanti, senza cambiare strategia. Senza intervento esaurisce
    # il budget iter. Soglia hardcoded 3 (basso ma sensato: 3 stessi fallimenti
    # = serve cambiare approccio, non riprovare).
    _repeat_cmd, _repeat_count = _detect_repeated_failed_command(messages, lookback=12)
    _repeated_cmd_nudge_sent = bool(state.get("repeated_cmd_nudge_sent") or False)
    if _repeat_cmd and _repeat_count >= 3 and not _repeated_cmd_nudge_sent:
        _cmd_text = (
            f"[LOOP RILEVATO] Hai eseguito `{_repeat_cmd[:120]}` "
            f"{_repeat_count} volte consecutive con errore. Continuare a "
            f"ripetere lo stesso comando non risolvera' il problema. "
            f"CAMBIA STRATEGIA ORA: esamina l'output dell'errore, identifica "
            f"la causa radice (dipendenza mancante? package rinominato? config "
            f"errata?), e prova un approccio diverso (es. tool diverso, comando "
            f"alternativo, lettura della doc, oppure chiedi all'utente)."
        )
        _cmd_nudge = HumanMessage(content=_cmd_text)
        messages = list(messages) + [_cmd_nudge]
        _repeated_cmd_nudge_sent = True
        logger.warning(
            "executor_node: nudge anti-loop-comando iniettato (cmd='%s', count=%d)",
            _repeat_cmd[:80], _repeat_count,
        )

    # ── Forced text response (anti-loop tool-only) ────────────────────────
    # Se il loop ha gia' consumato la maggior parte delle iterazioni concesse
    # (>= MAX_AGENT_ITERATIONS - 5) e il modello sta ancora facendo tool calls
    # (stop_reason precedente era "tool_use"), svuotiamo temporaneamente i tool
    # per forzare una risposta testuale nell'ultima finestra di iterazioni.
    # Questo previene loop in cui modelli small (es. Mistral) fanno tool calls
    # continue senza mai produrre testo, consumando tutte le iterazioni senza
    # lasciare una risposta utile all'utente.
    # ── Forza-azione (anti "pianifica e si ferma") ───────────────────────────
    # Se l'agente ha gia' esplorato oltre la soglia (consecutive_exploration_calls
    # >= soglia), rimuoviamo i tool di SOLA lettura dal set passato al modello:
    # cosi' e' costretto a usare i tool PRODUTTIVI (write_file/edit_file/
    # run_command/...) o a rispondere, invece di continuare a leggere/cercare e
    # annunciare azioni che poi non esegue (incidente osservato: l'agente
    # inventaria i componenti e chiude con "ora li creo" senza scriverli). Il
    # nudge testuale sopra da' la direzione; questo toglie l'opzione di esplorare
    # ancora. Solo se restano tool produttivi (non svuotiamo del tutto).
    if tools_json and _exploration_count >= _exploration_threshold:
        _productive_tools = [
            t for t in tools_json if t.get("name") not in _EXPLORATION_ONLY_TOOLS
        ]
        if _productive_tools and len(_productive_tools) < len(tools_json):
            logger.warning(
                "executor_node: forza-azione — rimossi %d tool di sola lettura "
                "(esplorazioni=%d >= soglia=%d), restano %d tool produttivi",
                len(tools_json) - len(_productive_tools),
                _exploration_count, _exploration_threshold, len(_productive_tools),
            )
            tools_json = _productive_tools

    _current_iterations = int(state.get("iterations") or 0)
    # Mig 0181: soglia forced-text proporzionale al budget adattivo del run.
    _iter_cap = int(state.get("iteration_budget") or 0) or MAX_AGENT_ITERATIONS
    _FORCED_TEXT_THRESHOLD = _iter_cap - 5
    _prev_stop_reason = state.get("stop_reason")
    if (
        tools_json
        and _current_iterations >= _FORCED_TEXT_THRESHOLD
        and _prev_stop_reason == "tool_use"
    ):
        logger.warning(
            "executor_node: forced text response — rimozione tool per produrre risposta "
            "testuale (iterations=%d >= threshold=%d, prev_stop=%s)",
            _current_iterations, _FORCED_TEXT_THRESHOLD, _prev_stop_reason,
        )
        tools_json = []

    # Fix M61 (sticky cascade): se il turno precedente ha fatto cascade fallback
    # con successo a un altro provider, "sticky" su quello nelle iter successive
    # invece di ripartire dal provider primario fallito. Lo sticky ha priorita'
    # ANCHE sul provider_override (l'utente sceglie un primario ma se quel
    # primario fallisce, il cascade-fallback diventa la nuova fonte di verita').
    sticky_provider = state.get("sticky_provider")
    sticky_model = state.get("sticky_model")
    provider = sticky_provider or state.get("provider_override")
    model = sticky_model or state.get("model_override")
    if not provider or not model:
        if _router is not None:
            # Passa anche il message originale: il router lo usa per detection
            # task rischiosi (override automatico a behavior_mode "approfondita"
            # se rileva verbi distruttivi: rm -rf, drop table, docker prune, ecc.)
            _last_msg_text = messages[-1].content if messages else ""
            # Floor selettivo: un task agentico "pesante" (agentic_score/budget alto)
            # in modalita' veloce/economica viene elevato a un tier tool-robust, cosi'
            # il loop tool-use non cade su un modello lite. Non tocca sticky/override.
            effective_mode = apply_agentic_tier_floor(behavior_mode, state)
            decision = _router.route_model(intent, token_budget, effective_mode, message=str(_last_msg_text))
            provider = provider or decision.provider
            model = model or decision.model
            logger.info("executor_node routing: %s", decision.rationale)
        else:
            # Router non disponibile: risolvi da DB (nexus_purpose_model).
            # Niente fallback hardcoded.
            try:
                from brain.router.service import _routing_client_singleton
                decision = _routing_client_singleton().purpose_model(purpose="agent_tier_sonnet")
                provider = provider or decision.provider
                model = model or decision.model
            except Exception as e:
                logger.error("executor_node: impossibile risolvere modello da DB: %s", e)
                raise RuntimeError(
                    "Nessun router disponibile e nexus_purpose_model non raggiungibile. "
                    "Verifica che mcp-core sia attivo e la migrazione 0102 sia applicata."
                ) from e

        # Sentinella gate (ADR 0020): la risoluzione automatica (route_model o
        # purpose_model) puo' ritornare __no_capable_provider__ quando TUTTI i
        # provider capable sono in cooldown billing/quota, o __router_unavailable__
        # se il gate e' giu'. NON proseguire chiamando un provider morto: fermarsi
        # con errore chiaro (no blocco silenzioso). Lo sticky/override utente,
        # risolto sopra, non passa mai di qui (e' una scelta esplicita, vincolante).
        if provider in ("__no_capable_provider__", "__router_unavailable__") or model in (
            "__no_capable_provider__",
            "__router_unavailable__",
        ):
            raise RuntimeError(
                "Nessun provider AI disponibile (tutti i provider capable sono in "
                f"cooldown billing/quota oppure il gate di routing non risponde): "
                f"provider={provider}. Il run si ferma invece di ritentare provider morti "
                "(ADR 0020). Riprova quando un provider torna disponibile."
            )

    logger.info(
        "executor_node: provider=%s model=%s intent=%s tools=%d",
        provider, model, intent, len(tools_json),
    )
    # Nexus thinking: lista locale, popolata lungo l'executor; emessa nel
    # return finale (backward-compat / final state) E in tempo reale via
    # _stream_thinking_live (custom stream LangGraph) cosi' l'utente vede i
    # ragionamenti SCORRERE durante l'attesa, non tutti insieme a fine nodo.
    _executor_thinking: list[str] = []
    _thinking_on = _nexus_thinking_enabled()

    def _think(line: str) -> None:
        """Accoda una riga di thinking E la emette live sul custom stream.

        Unico punto di emissione thinking dell'executor: garantisce che ogni
        ragionamento esca immediatamente (live) e resti anche nel return finale
        per il delta di fine nodo (consumer SSE storico).
        """
        if not line:
            return
        txt = str(line).strip()
        if not txt:
            return
        _executor_thinking.append(txt)
        if _thinking_on:
            _stream_thinking_live(txt)

    _think(
        f"Routing modello: {provider}/{model} (intent {intent}, tools disponibili {len(tools_json)})."
    )
    # FIX 2: ragionamento d'ingresso prima della chiamata LLM, emesso live.
    _think(
        f"Iterazione {_current_iterations}: consulto il modello {provider}/{model}..."
    )

    # ── G1 Python: nudge anti-descrittivo ─────────────────────────────────────
    # Se il modello ha già risposto almeno una volta (iterations >= 1) senza
    # chiamare NESSUN tool (risposta puramente descrittiva) e la richiesta
    # originale era un'azione concreta (avvia, installa, crea, docker, ...)
    # inietta un messaggio di nudge PRIMA di chiamare il LLM.
    # Cap: max 2 nudge per run (action_nudge_count in state) per evitare loop.
    _current_iter = int(state.get("iterations") or 0)
    _nudge_count = int(state.get("action_nudge_count") or 0)
    if (
        tools_json                                        # l'agente ha tool disponibili
        and _current_iter >= 1                            # route_after_executor ha già ri-mandato qui
        and _nudge_count < 2                              # max 2 nudge totali
        and not _has_tool_calls_in_history(messages)     # nessun tool call nella history
    ):
        # Estrai il primo messaggio umano per capire se è action-oriented
        _first_human_text = next(
            (getattr(m, "content", "") for m in messages if hasattr(m, "type") and m.type == "human"),
            "",
        )
        if isinstance(_first_human_text, list):
            _first_human_text = " ".join(
                b.get("text", "") for b in _first_human_text if isinstance(b, dict)
            )
        # G1 esteso: il nudge scatta sia quando la richiesta utente e' un'azione
        # concreta (input action-oriented) sia quando il modello ha appena
        # ANNUNCIATO un'azione imminente senza eseguirla (output con intenzione
        # non compiuta — "Inizio verificando index.html" e poi chiude). Questo
        # secondo caso copre i debug/diagnosi dove il primo messaggio non e'
        # imperativo ma l'agente narra il piano invece di agire.
        _is_action_req = _detect_action_request(str(_first_human_text))
        _last_asst_text = _last_assistant_text(messages)
        _is_unfulfilled = _detect_unfulfilled_intent(_last_asst_text)
        _is_polling = _detect_polling_wait(_last_asst_text)
        if _is_action_req or _is_unfulfilled:
            if _is_polling:
                # Anti wait-loop: l'agente sta aspettando passivamente uno stato
                # che potrebbe non cambiare (container/servizio in crash-loop).
                # Invece di ri-attendere, deve DIAGNOSTICARE la causa.
                _nudge_content = (
                    "STOP: stai aspettando passivamente uno stato che potrebbe non "
                    "cambiare (es. container o servizio in crash-loop). NON attendere "
                    "e ricontrollare di nuovo: DIAGNOSTICA ORA la causa con un tool. "
                    "Leggi i log del servizio/container che non parte (run_command: "
                    "`docker logs`, `docker compose logs`, `journalctl -u <unit>`), "
                    "individua l'errore reale e agisci sulla causa. "
                    "Esegui subito il comando diagnostico con un tool call."
                )
                _nudge_reason = "wait-loop(polling)"
            else:
                _nudge_content = (
                    "⚠️ ERRORE: hai annunciato/descritto cosa avresti fatto, "
                    "ma NON hai chiamato nessun tool. Questo non è accettabile. "
                    "AGISCI ADESSO — esegui l'azione che hai appena dichiarato "
                    "usando un tool: shell_exec/run_command per comandi (docker, "
                    "npm, dotnet, ss, ecc.), read_file/list_files per ispezionare, "
                    "write_file/edit_file per creare o modificare file. "
                    "Nessuna spiegazione: ESEGUI il prossimo step concreto con un tool call."
                )
                _nudge_reason = (
                    "action-request" if _is_action_req else "intent-non-compiuta"
                )
            _nudge_msg = HumanMessage(content=_nudge_content)
            messages = list(messages) + [_nudge_msg]
            logger.warning(
                "G1 nudge iniettato (iter=%d, nudge_count=%d, intent=%s, motivo=%s)",
                _current_iter, _nudge_count, intent, _nudge_reason,
            )

    # Heartbeat A: pubblica step "calling_model" cosi' l'utente vede in chat
    # che l'agente sta lavorando (LLM call puo' durare 30-60s su contesti grandi).
    try:
        _calling_meta = meta_steps.make(
            kind="executor_call",
            title=f"Sto interrogando {provider}/{model}",
            payload={
                "provider": provider,
                "model": model,
                "intent": intent,
                "iteration": state.get("iterations", 0),
                "tools_count": len(tools_json),
            },
        )
        if _calling_meta:
            meta_steps.persist_async(state.get("thread_id"), _calling_meta)
    except Exception as _hb_exc:
        logger.debug("executor_node: heartbeat calling_model fallito: %s", _hb_exc)

    start_ms = time.monotonic() * 1000
    created_at = datetime.datetime.now(datetime.timezone.utc).isoformat()
    result_text = ""
    token_usage = 0
    prompt_tokens = 0
    completion_tokens = 0
    cache_creation_tokens = 0
    cache_read_tokens = 0
    total_cost_usd = 0.0
    temperature = 0.2  # default Anthropic
    top_p = 0.9  # default
    pending_tool_uses: list[dict] = []
    stop_reason: str | None = "end_turn"
    assistant_msg: AIMessage | None = None

    if _providers is None:
        result_text = "[Servizi non configurati]"
    elif tools_json:
        ctx_size = _estimate_context_chars(messages)
        # ── Rolling summarization (BP4) ──────────────────────────────────────
        # Se siamo oltre il 60% di MAX_CONTEXT_CHARS, prima di troncare
        # tool_result tentiamo di riassumere i messaggi vecchi con un modello
        # small. Cosi' preserviamo le decisioni e gli errori passati invece
        # di perderli con il truncation. Best-effort: se fallisce, fallback
        # alla compressione standard.
        from brain.agents import summarizer as _summarizer
        if _summarizer.should_trigger_summary(ctx_size, MAX_CONTEXT_CHARS):
            try:
                summarized = await _summarizer.summarize_old_messages(
                    messages,
                    providers=_providers,
                    keep_recent=_summarizer.DEFAULT_KEEP_RECENT,
                    thread_id=str(state.get("thread_id") or ""),
                )
                if summarized is not None:
                    messages = summarized
                    new_size = _estimate_context_chars(messages)
                    logger.info(
                        "executor_node: rolling summary %d -> %d char",
                        ctx_size, new_size,
                    )
                    ctx_size = new_size
            except Exception as exc:
                logger.warning("executor_node: summarizer fallito: %s", exc)
        # ── FIX A-C (ADR 0014): context size management ──────────────────────
        # Pipeline ordinata:
        #   1) FIX B: dedup tool_result identici per signature (tool_name+args).
        #   2) FIX C: drop body base64 vecchi non citati dalla history.
        #   3) FIX A: compressione escalante anticipata da iter compress_start_iter
        #      (default 5). I parametri keep_recent / max_content_chars sono
        #      DB-driven (mig 0199).
        # Tutto in cache 60s via _load_ctx_mgmt_config().
        _ctx_cfg = _load_ctx_mgmt_config()
        _compress_iter = _current_iterations

        # FIX B: dedup tool_result identici per signature.
        if _ctx_cfg.get("dedup_tool_results_enabled", True):
            _pre_dedup_size = ctx_size
            messages = _dedup_tool_results_history(messages)
            _post_dedup = _estimate_context_chars(messages)
            if _post_dedup != _pre_dedup_size:
                logger.info(
                    "executor_node: FIX B dedup signature iter=%d: %d -> %d char",
                    _compress_iter, _pre_dedup_size, _post_dedup,
                )
                ctx_size = _post_dedup

        # FIX C: drop base64 vecchi non citati.
        _pre_drop = ctx_size
        messages = _drop_unused_base64_payloads(messages)
        _post_drop = _estimate_context_chars(messages)
        if _post_drop != _pre_drop:
            logger.info(
                "executor_node: FIX C drop base64 iter=%d: %d -> %d char",
                _compress_iter, _pre_drop, _post_drop,
            )
            ctx_size = _post_drop

        # FIX A: compressione escalante anticipata.
        _compress, _params = _should_compress_now(_compress_iter, _ctx_cfg)
        if _compress:
            messages = _compress_old_tool_results(
                messages,
                keep_recent=_params["keep_recent"],
                max_content_chars=_params["max_content_chars"],
            )
            new_size = _estimate_context_chars(messages)
            logger.info(
                "executor_node: FIX A compressione iter=%d keep_recent=%d "
                "max_content_chars=%d: %d -> %d char",
                _compress_iter, _params["keep_recent"], _params["max_content_chars"],
                ctx_size, new_size,
            )
            ctx_size = new_size
        elif ctx_size > MAX_CONTEXT_CHARS // 2:
            # Safety net storica: contesto >50% MAX anche pre-iter=start.
            messages = _compress_old_tool_results(messages, keep_recent=6)
            new_size = _estimate_context_chars(messages)
            logger.info(
                "executor_node: contesto compresso (safety net) %d -> %d char",
                ctx_size, new_size,
            )
        # ── ADR 0016 Fase A.3: rolling summary cross-turno ───────────────────
        # Ogni N turni offloada i messaggi vecchi in Qdrant chat_history_rolling
        # e li sostituisce con un summary compatto. Originali retrievabili via
        # nexus_search_semantic(source_kinds=chat_history). Best-effort: se
        # offload fallisce, no compressione (degrada ai brake successivi).
        try:
            messages = _apply_rolling_summary(messages, _current_iterations, _embeddings)
        except Exception as _roll_exc:
            logger.warning("rolling_summary best-effort fallita: %s", _roll_exc)

        # ── ADR 0016 Fase C: smart upscale modello ───────────────────────────
        # Se il context stimato supera il window del modello attivo, escalation
        # a modello con window maggiore (gemini-2.5-pro / claude-opus-4-6 / ...).
        # PRIMA del brake: cosi' il brake usa il window del modello effettivo,
        # non di quello iniziale. Switch tracciato in agent_runs (Fase C UI).
        try:
            _upscale_pre_tokens = _estimate_context_tokens(messages)
            _upscale_window = _model_context_window(model)
            _upscale_result = _smart_upscale_model(model, _upscale_window, _upscale_pre_tokens)
            if _upscale_result is not None:
                _orig_model = model
                model, _upscale_reason = _upscale_result
                provider = _provider_from_model(model) or provider
                logger.info(
                    "executor_node: smart upscale %s -> %s (reason=%s, est=%d)",
                    _orig_model, model, _upscale_reason, _upscale_pre_tokens,
                )
        except Exception as _upscale_exc:
            logger.warning("smart upscale fallito (no switch): %s", _upscale_exc)

        # ── Freno TOKEN-based intra-turno (mig 0280 + ADR 0016 Fase D) ───────
        # Ultima barriera PRIMA di costruire i messaggi Anthropic: garantisce
        # che il context inviato al provider non superi mai il window (cap hard
        # come safety net). Usa tiktoken (Fase D) per stima accurata.
        messages = _apply_token_brake(messages, model, _ctx_cfg, _current_iterations)
        # Bug #88: reminder lingua resiliente al contesto/profilo. Iniettato qui
        # cosi' copre sia la chiamata principale (generate_agent_turn_sync con
        # system_text) sia il fallback anti-loop (system_text2 = system_text +
        # anti_loop_hint, che eredita il reminder dal system_text base). Doppia
        # iniezione: garanzia nel system + recency sull'ultimo HumanMessage.
        # ── ADR 0016 Fase A.1: offload preventivo system prompt ─────────────
        # Se il system prompt supera threshold token, indicizziamo in Qdrant
        # (collection tool_results_chunks, source_kind=system_context) e nel
        # prompt resta head + pointer esplicito. L'agente recupera con
        # nexus_search_semantic(source_kinds=system_context) on-demand.
        try:
            system_text = _offload_system_prompt_if_huge(system_text, _embeddings)
        except Exception as _offload_exc:
            logger.warning("system_prompt offload best-effort fallita: %s", _offload_exc)

        _lang_enabled, _lang_text = _load_language_reminder()
        messages, system_text = _inject_language_reminder(
            messages, system_text, _lang_enabled, _lang_text
        )
        # ── ADR 0016 Fase A.4: forced RAG reminder ───────────────────────────
        # Quando il context stimato supera forced_rag_threshold_ratio*window,
        # iniettiamo un'istruzione assertiva: l'agente deve usare
        # nexus_search_semantic prima di rispondere, invece di assumere visione
        # completa del context. Riduce in modo strutturale i casi di context
        # overflow (vedi chat 6 incident: 1.2M token mandati a window 131k).
        # Usa _estimate_context_tokens (tiktoken, Fase D) per stima accurata.
        _rag_est_tokens = _estimate_context_tokens(messages)
        _rag_window = _model_context_window(model)
        messages, system_text = _inject_forced_rag_reminder(
            messages, system_text, _rag_est_tokens, _rag_window
        )
        anth_messages = _langchain_to_anthropic_messages(messages)
        # ── ADR 0018 (b): decisione tool_choice forcing (funzione pura) ───────
        # Forziamo una tool call nei turni d'azione iniziali cosi' il modello
        # NON puo' chiudere narrando senza eseguire (stop narrativo alla radice).
        # La decisione e' una funzione pura testabile + config DB (regola G).
        _force_tc: bool | None = None
        try:
            from .helpers import (
                _load_tool_choice_forcing_config,
                provider_style_supports_forcing,
                should_force_tool_choice,
            )
            _tc_enabled, _tc_max_iter = _load_tool_choice_forcing_config()
            # action_oriented: riusa il rilevamento sul primo messaggio umano.
            _first_human_tc = next(
                (getattr(m, "content", "") for m in messages if hasattr(m, "type") and m.type == "human"),
                "",
            )
            if isinstance(_first_human_tc, list):
                _first_human_tc = " ".join(
                    b.get("text", "") for b in _first_human_tc if isinstance(b, dict)
                )
            _action_oriented_tc = _detect_action_request(str(_first_human_tc))
            # in_discovery_phase: turno M16 dove esponiamo solo il meta-tool di
            # search (il forcing della search e' gia' gestito separatamente).
            _names_tc = {t.get("name") for t in tools_json if isinstance(t, dict)}
            _in_discovery = bool(_names_tc) and _names_tc <= {"nexus_mcp_tool_search"}
            # provider_supports_forcing: dallo style della capability del modello.
            _supports_forcing = False
            try:
                from brain.providers.capability_loader import load_capability
                _cap_tc = load_capability(provider, model)
                _supports_forcing = provider_style_supports_forcing(
                    getattr(_cap_tc, "tool_choice_style", None)
                )
            except Exception:
                _supports_forcing = False
            if should_force_tool_choice(
                tools_available=bool(tools_json),
                action_oriented=_action_oriented_tc,
                iteration=_current_iterations,
                in_discovery_phase=_in_discovery,
                provider_supports_forcing=_supports_forcing,
                enabled=_tc_enabled,
                max_iteration=_tc_max_iter,
            ):
                _force_tc = True
                logger.info(
                    "executor_node: tool_choice forcing attivo (iter=%d, max=%d, provider=%s)",
                    _current_iterations, _tc_max_iter, provider,
                )
        except Exception as _tc_exc:
            logger.warning("executor_node: decisione tool_choice forcing saltata (%s)", _tc_exc)
            _force_tc = None
        try:
            # max_tokens dinamico: almeno 8192 per turni con tool, capped a 16384.
            # Il token_budget dallo state (stimato dal router_node) viene usato
            # come base, ma per agent turn con tool serve molto piu' spazio
            # (tool_use JSON e' verboso, content_json per documenti puo' essere enorme).
            effective_max_tokens = max(8192, min(token_budget * 4, 16384))
            # Fix M60: passa dal registry per riusare la cascade fallback esistente
            # (provider_hierarchy + nexus_provider_default_model + classify_error).
            # generate_agent_turn_sync e' bloccante, lo runno in thread.
            prov_result = await asyncio.to_thread(
                _providers.generate_agent_turn_sync,
                provider, model, anth_messages, tools_json,
                max_tokens=effective_max_tokens, system_text=system_text,
                usage_run_id=str(state.get("thread_id") or ""),
                usage_iteration=_current_iterations,
                usage_intent=str(state.get("user_intent") or ""),
                force_tool_choice=_force_tc,
            )
            # ── ADR 0018 (b): retry-senza-forcing su errore di forcing ────────
            # Se il forcing ha causato un errore provider (es.
            # MALFORMED_FUNCTION_CALL o modello non tool-capable), ritentiamo
            # UNA volta SENZA forcing per non far fallire il run. Niente
            # payload/prompt nei log (regola F).
            if _force_tc is True:
                _meta_chk = prov_result.metadata or {}
                _err_chk = str(_meta_chk.get("error") or "")
                _stop_chk = _meta_chk.get("stop_reason")
                _forcing_failed = (
                    _stop_chk == "error"
                    and (
                        "MALFORMED_FUNCTION_CALL" in _err_chk
                        or "tool_choice" in _err_chk.lower()
                        or "function_call" in _err_chk.lower()
                    )
                )
                if _forcing_failed:
                    logger.warning(
                        "executor_node: tool_choice forcing ha causato errore provider "
                        "(%s/%s), retry SENZA forcing per questo turno",
                        provider, model,
                    )
                    prov_result = await asyncio.to_thread(
                        _providers.generate_agent_turn_sync,
                        provider, model, anth_messages, tools_json,
                        max_tokens=effective_max_tokens, system_text=system_text,
                        usage_run_id=str(state.get("thread_id") or ""),
                        usage_iteration=_current_iterations,
                        usage_intent=str(state.get("user_intent") or ""),
                        force_tool_choice=False,
                    )
            # Aggiorna provider/model effettivamente usati se la cascade ha fatto fallback.
            # Salva anche come "sticky" per le iter successive (M61): evita di ri-tentare
            # il provider primario fallito ad ogni round, risparmiando latenza/cost.
            cascade_did_fallback = False
            if prov_result.provider and prov_result.provider != provider:
                logger.info(
                    "executor_node: cascade fallback %s -> %s/%s (sticky per iter successive)",
                    provider, prov_result.provider, prov_result.model,
                )
                provider = prov_result.provider
                model = prov_result.model
                cascade_did_fallback = True
                # Fix M62: propaga il provider/model effettivi anche su agent_runs cosi'
                # la chat UI mostra il fallback invece del primario fallito. Best-effort:
                # se il DB e' down logghiamo soltanto.
                try:
                    import os, psycopg2  # type: ignore[import-untyped]
                    _run_id = str(state.get("thread_id") or "")
                    _dburl = os.environ.get("DATABASE_URL")
                    if _run_id and _dburl:
                        _conn = psycopg2.connect(_dburl)
                        with _conn.cursor() as _cur:
                            _cur.execute(
                                "UPDATE agent_runs SET provider=%s, model=%s WHERE id=%s",
                                (provider, model, _run_id),
                            )
                        _conn.commit()
                        _conn.close()
                except Exception as _exc:
                    logger.warning("executor_node: UPDATE agent_runs cascade fallita: %s", _exc)
            result_text = prov_result.content or ""
            meta = prov_result.metadata or {}
            stop_reason = meta.get("stop_reason") or "end_turn"
            pending_tool_uses = list(meta.get("tool_use_blocks") or [])
            assistant_content = meta.get("assistant_content")
            # Heartbeat A: pubblica step "tool_calls" con elenco tool richiesti.
            if pending_tool_uses:
                try:
                    _tools_meta = meta_steps.make(
                        kind="tool_calls",
                        title=f"Sto usando {len(pending_tool_uses)} tool",
                        payload={
                            "tools": [
                                {
                                    "name": tu.get("name"),
                                    "input_keys": list((tu.get("input") or {}).keys())[:8],
                                }
                                for tu in pending_tool_uses[:10]
                            ],
                            "iteration": _current_iterations,
                        },
                    )
                    if _tools_meta:
                        meta_steps.persist_async(state.get("thread_id"), _tools_meta)
                except Exception as _hb_exc:
                    logger.debug("executor_node: heartbeat tool_calls fallito: %s", _hb_exc)
            usage = meta.get("usage") or {}
            prompt_tokens = int(usage.get("input_tokens", 0))
            completion_tokens = int(usage.get("output_tokens", 0))
            cache_creation_tokens = int(usage.get("cache_creation_input_tokens", 0))
            cache_read_tokens = int(usage.get("cache_read_input_tokens", 0))
            token_usage = prompt_tokens + completion_tokens

            input_price, output_price = _lookup_price(provider, model)
            if input_price > 0 or output_price > 0:
                cache_write_price = input_price * 1.25
                cache_read_price = input_price * 0.1
                total_cost_usd = (
                    (prompt_tokens * input_price) / 1_000_000.0 +
                    (completion_tokens * output_price) / 1_000_000.0 +
                    (cache_creation_tokens * cache_write_price) / 1_000_000.0 +
                    (cache_read_tokens * cache_read_price) / 1_000_000.0
                )

            # Preserviamo il content strutturato per il prossimo round.
            assistant_msg = AIMessage(
                content=result_text,
                additional_kwargs={"anthropic_content": assistant_content} if assistant_content else {},
            )

            # NB: l'usage di questo turno e' gia' registrato UNA volta dentro
            # generate_agent_turn_sync (feature neural.GenerateAgentTurn, con
            # run_id/iteration passati sopra), inclusa l'eventuale chiamata di
            # fallback. NON registrarlo di nuovo qui: la doppia registrazione
            # gonfiava il consumo riportato di ~2x (regola H, fix definitivo).

        except Exception as exc:
            logger.error("executor_node: agent_turn %s/%s: %s", provider, model, exc)
            result_text = f"[Errore provider {provider}: {exc}]"
            stop_reason = "error"
    else:
        # Percorso legacy single-shot.
        last_text = ""
        if messages:
            last = messages[-1]
            last_text = last.content if hasattr(last, "content") else str(last)
        try:
            prov_result = await _providers.generate_completion_async(
                provider, model, last_text
            )
            result_text = prov_result.content
            usage = prov_result.metadata.get("usage", {})
            prompt_tokens = int(usage.get("input_tokens", 0))
            completion_tokens = int(usage.get("output_tokens", 0))
            cache_read_tokens = int(usage.get("cache_read_input_tokens", 0))
            token_usage = prompt_tokens + completion_tokens

            input_price, output_price = _lookup_price(provider, model)
            if input_price > 0 or output_price > 0:
                cache_read_price = input_price * 0.1
                total_cost_usd = (
                    (prompt_tokens * input_price) / 1_000_000.0 +
                    (completion_tokens * output_price) / 1_000_000.0 +
                    (cache_read_tokens * cache_read_price) / 1_000_000.0
                )
        except Exception as exc:
            logger.error("executor_node: completion %s/%s: %s", provider, model, exc)
            result_text = f"[Errore provider {provider}: {exc}]"

    if assistant_msg is None:
        assistant_msg = AIMessage(content=result_text)

    latency_ms = time.monotonic() * 1000 - start_ms

    # ── Loop detection ────────────────────────────────────────────────────
    # Se il modello ripete la stessa tool call (stesso name + stesso input)
    # 3 volte di fila, abortiamo con messaggio chiaro: il modello e' bloccato.
    new_signatures: list[str] = []
    for tu in pending_tool_uses:
        sig_input = json.dumps(tu.get("input") or {}, sort_keys=True, ensure_ascii=False)
        sig = f"{tu.get('name', '')}|{hashlib.sha1(sig_input.encode()).hexdigest()[:12]}"
        new_signatures.append(sig)
    recent: list[str] = list(state.get("recent_tool_signatures") or [])
    combined = recent + new_signatures
    LOOP_THRESHOLD = 3
    loop_sig: str | None = None
    if len(combined) >= LOOP_THRESHOLD and new_signatures:
        for sig in new_signatures:
            tail = [s for s in combined[-LOOP_THRESHOLD * 2:] if s == sig]
            if len(tail) >= LOOP_THRESHOLD:
                loop_sig = sig
                break
    if loop_sig is not None:
        tool_name = loop_sig.split("|", 1)[0]
        logger.warning(
            "executor_node: LOOP detected %s/%s ripete tool='%s' (signature=%s) %d+ volte. Abort.",
            provider, model, tool_name, loop_sig, LOOP_THRESHOLD,
        )
        # Auto-escalation: al primo loop proviamo automaticamente un modello più capace.
        # Priorità:
        #   1. Catena intra-provider (nexus_model_escalation_chain) — stesso provider, tier superiore
        #   2. Purpose model DB-driven dal router Rust (loop_fallback_default)
        #   3. Nessun fallback hardcoded: se nulla disponibile, segna loop_detected
        escalations = int(state.get("auto_escalations") or 0)
        tried_escalation = False
        if escalations < 3 and _providers is not None and tools_json:
            try:
                fallback_provider: str | None = None
                fallback_model: str | None = None

                # Cooldown gate (ADR 0020): se il provider corrente e' in
                # cooldown billing/quota secondo il gate Rust (fonte unica),
                # NON tentare la catena intra-provider (Tier 1) che resterebbe
                # sullo stesso provider morto. Si salta direttamente al Tier 2
                # cross-provider, gia' filtrato dal gate.
                _cooldown_set: set[str] = set()
                try:
                    from brain.router.service import _routing_client_singleton as _rcs
                    _cd = _rcs().cooldown_providers()
                    if _cd is not None:
                        _cooldown_set = _cd
                except Exception:
                    _cooldown_set = set()
                _provider_in_cooldown = (provider or "").strip().lower() in _cooldown_set

                # === Tier 1: catena intra-provider ===
                try:
                    import psycopg2  # type: ignore[import]
                    import os as _os
                    _db_url = _get_db_url()
                    with psycopg2.connect(_db_url) as _conn:
                        with _conn.cursor() as _cur:
                            _cur.execute(
                                "SELECT escalation_model FROM nexus_model_escalation_chain "
                                "WHERE provider = %s AND base_model = %s AND is_active = TRUE "
                                "ORDER BY escalation_position ASC LIMIT %s",
                                (provider, model, escalations + 1),
                            )
                            _rows = _cur.fetchall()
                    if _rows and len(_rows) > escalations and not _provider_in_cooldown:
                        # Prende la posizione corrispondente al numero di escalation già fatte
                        candidate_model = _rows[escalations][0]
                        if _providers._providers.get(provider):  # type: ignore[attr-defined]
                            fallback_provider = provider
                            fallback_model = candidate_model
                            logger.info(
                                "executor_node: LOOP catena intra-provider pos=%d %s/%s => %s/%s",
                                escalations + 1, provider, model, provider, candidate_model,
                            )
                except Exception as _e:
                    logger.debug("executor_node: catena escalation DB fallita: %s", _e)

                # === Tier 2: purpose model dal router Rust (cross-provider) ===
                if not fallback_provider:
                    try:
                        from brain.router.service import _routing_client_singleton
                        decision2 = _routing_client_singleton().purpose_model(purpose="loop_fallback_default")
                        if decision2.provider not in ("__router_unavailable__", "__no_capable_provider__"):
                            fallback_provider = decision2.provider
                            fallback_model = decision2.model
                            logger.info(
                                "executor_node: LOOP -> purpose_model fallback %s/%s => %s/%s",
                                provider, model, fallback_provider, fallback_model,
                            )
                    except Exception:
                        pass

                if fallback_provider and fallback_model:
                    p2 = _providers._providers.get(fallback_provider)  # type: ignore[attr-defined]
                    if p2 is not None and hasattr(p2, "generate_agent_turn"):
                        # Hint anti-loop nel system_text: chiede esplicitamente di NON ripetere il tool.
                        anti_loop_hint = (
                            "\n\n[ANTI-LOOP] Hai appena ripetuto la stessa tool call "
                            f"('{tool_name}' con stesso input) più volte. "
                            "Non ripetere la stessa tool call con lo stesso input. "
                            "Se mancano informazioni, fai UNA richiesta più specifica "
                            "oppure cambia strategia e riassumi lo stato."
                        )
                        system_text2 = (system_text or "") + anti_loop_hint

                        # Riprova lo stesso turno agente con provider/model più capace.
                        prov2 = await p2.generate_agent_turn(
                            fallback_model,
                            anth_messages,
                            tools_json,
                            max_tokens=effective_max_tokens,
                            system_text=system_text2,
                        )
                        result_text = prov2.content or ""
                        meta2 = prov2.metadata or {}
                        stop_reason = meta2.get("stop_reason") or "end_turn"
                        pending_tool_uses = list(meta2.get("tool_use_blocks") or [])
                        assistant_content2 = meta2.get("assistant_content")
                        if assistant_content2:
                            assistant_msg = AIMessage(
                                content="",
                                additional_kwargs={"anthropic_content": assistant_content2},
                            )
                        else:
                            assistant_msg = AIMessage(content=result_text)

                        provider = fallback_provider
                        model = fallback_model
                        tried_escalation = True
                        new_signatures = []  # reset signature accumulator dopo escalation
            except Exception as exc:
                logger.warning("executor_node: auto-escalation fallita: %s", exc)

        if not tried_escalation:
            loop_msg = (
                f"[LOOP RILEVATO] Il modello {provider}/{model} ha ripetuto il tool "
                f"'{tool_name}' con stesso input {LOOP_THRESHOLD}+ volte senza progresso. "
                f"Esecuzione interrotta per evitare stallo. "
                f"Suggerimento: usa un modello piu' capace (es. anthropic/claude) o "
                f"riformula il prompt in modo piu' specifico."
            )
            assistant_msg = AIMessage(content=loop_msg)
            pending_tool_uses = []
            stop_reason = "loop_detected"
            result_text = loop_msg
            new_signatures = []  # reset, non accumulare ulteriormente
        else:
            # Persisti lo stato di escalation per evitare retry infiniti.
            escalations += 1

    # Mantieni solo le ultime ~12 signature per non gonfiare lo state.
    updated_signatures = (recent + new_signatures)[-12:]

    # ── Aggiorna contatore loop-detection semantica ───────────────────────────
    # Conta le tool call di SOLA esplorazione del turno corrente. Se TUTTE le
    # call pending sono esplorative, accumula (il modello sta ancora leggendo).
    # Se almeno una e' PRODUTTIVA (qualunque tool NON in _EXPLORATION_ONLY_TOOLS,
    # es. write_file/edit_file/run_command/request_port), azzera contatore e
    # flag nudge: il modello ha iniziato a produrre. Senza tool call (risposta
    # testuale) il contatore resta invariato.
    _updated_exploration_count = int(state.get("consecutive_exploration_calls") or 0)
    _updated_exploration_nudge_sent = (
        _exploration_nudge_sent if _exploration_nudge_injected
        else bool(state.get("exploration_nudge_sent") or False)
    )
    if pending_tool_uses:
        _pending_names = [str(tu.get("name", "")) for tu in pending_tool_uses]
        _all_exploration = all(n in _EXPLORATION_ONLY_TOOLS for n in _pending_names)
        if _all_exploration:
            _updated_exploration_count += len(_pending_names)
        else:
            # Almeno una call produttiva: il modello sta scrivendo, reset.
            _updated_exploration_count = 0
            _updated_exploration_nudge_sent = False

    # Calcola cache hit rate
    total_tokens = prompt_tokens + completion_tokens + cache_creation_tokens + cache_read_tokens
    cache_hit_rate = cache_read_tokens / total_tokens if total_tokens > 0 else 0.0

    # M61 sticky cascade: se nel turno corrente c'è stato fallback, salva i
    # provider/model effettivi nello state cosi' le iter successive partono
    # direttamente da li' senza ri-tentare il provider primario fallito.
    sticky_out = {
        "sticky_provider": provider if (locals().get("cascade_did_fallback") or state.get("sticky_provider")) else state.get("sticky_provider"),
        "sticky_model": model if (locals().get("cascade_did_fallback") or state.get("sticky_model")) else state.get("sticky_model"),
    }

    # Nexus thinking: descrivi azioni decise da questo turno, emesse LIVE.
    try:
        # FIX 2: testo intermedio del modello (il ragionamento che accompagna
        # le tool call). Se presente, emettiamo le prime ~200 char come thinking
        # cosi' l'utente vede il "perche'" dietro le azioni mentre scorrono.
        if result_text and pending_tool_uses:
            _intermediate = str(result_text).strip()
            if _intermediate:
                _snippet = _intermediate[:200]
                if len(_intermediate) > 200:
                    _snippet += "..."
                _think(_snippet)
        # FIX 2: per ogni tool, frase italiana leggibile derivata dagli args.
        for _tu in (pending_tool_uses or []):
            if isinstance(_tu, dict):
                _tu_name = _tu.get("name") or "sconosciuto"
                _tu_args = _tu.get("input") if isinstance(_tu.get("input"), dict) else _tu.get("args")
            else:
                _tu_name, _tu_args = "sconosciuto", None
            _think(_describe_tool_call(str(_tu_name), _tu_args))
        if stop_reason == "end_turn" and not pending_tool_uses:
            _think("Risposta finale generata (end_turn).")
    except Exception as _thinking_exc:
        logger.debug("executor_node: thinking append fallito: %s", _thinking_exc)

    _thinking_payload: dict[str, Any] = {}
    if _executor_thinking and _thinking_on:
        _thinking_payload["nexus_thinking"] = list(_executor_thinking)

    # ── Scelte di proseguimento (meta_step next_actions) ──────────────────────
    # SOLO a turno realmente concluso (end_turn senza tool pendenti): la risposta
    # assistant e' completa e visibile all'utente. Approccio ibrido (punto unico
    # in brain/agents/next_actions.py): (1) blocco <suggested_actions> emesso
    # dall'agente -> parse + rimozione dal testo visibile; (2) fallback LLM
    # leggero (purpose 'choices_extractor') se la risposta sembra proporre scelte.
    # Best-effort: qualunque errore non deve rompere il turno ne' lo streaming.
    _next_actions_payload: dict[str, Any] = {}
    if stop_reason == "end_turn" and not pending_tool_uses and result_text:
        try:
            from .. import next_actions as _next_actions
            _cleaned_text, _na_step = await _next_actions.derive(result_text, _providers)
            # Allinea testo visibile (result + AIMessage) al testo ripulito dal
            # blocco grezzo: il generator SSE emette `result` come assistant_delta,
            # quindi qui e' l'unico punto in cui togliere il blocco prima dell'UI.
            if _cleaned_text != result_text:
                result_text = _cleaned_text
                assistant_msg = AIMessage(
                    content=_cleaned_text,
                    additional_kwargs=getattr(assistant_msg, "additional_kwargs", {}) or {},
                )
            if _na_step is not None:
                _next_actions_payload["meta_steps"] = [_na_step]
                meta_steps.persist_async(state.get("thread_id"), _na_step)
        except Exception as _na_exc:
            logger.debug("executor_node: next_actions derive fallita: %s", _na_exc)

    # ── Resoconto onesto su intenzione non eseguita (confirm / no auto-restart) ─
    # Se il turno chiude annunciando un passo/attesa senza eseguirlo e NON ci
    # sara' auto-restart (modalita' confirm: l'utente vuole controllo
    # step-by-step), sostituiamo la "promessa monca" con un resoconto onesto
    # (cosa fatto, cosa manca, prossimo passo). In automatic/continuous il
    # re-entry G1 (route_after_executor) fa invece agire il modello: qui non
    # interveniamo. Esclude le richieste d'azione esplicite (gestite dal G1).
    if stop_reason == "end_turn" and not pending_tool_uses and result_text:
        _auto_mode = (state.get("automation_mode") or "confirm").strip().lower()
        if _auto_mode not in ("automatic", "continuous") and _detect_unfulfilled_intent(
            result_text
        ):
            _first_human_rep = next(
                (
                    getattr(m, "content", "")
                    for m in messages
                    if hasattr(m, "type") and m.type == "human"
                ),
                "",
            )
            if isinstance(_first_human_rep, list):
                _first_human_rep = " ".join(
                    b.get("text", "") for b in _first_human_rep if isinstance(b, dict)
                )
            if not _detect_action_request(str(_first_human_rep)):
                _report = build_unfulfilled_report(result_text, messages)
                result_text = _report
                assistant_msg = AIMessage(
                    content=_report,
                    additional_kwargs=getattr(assistant_msg, "additional_kwargs", {}) or {},
                )
                logger.info(
                    "executor_node: intenzione non eseguita in modalita' %s -> "
                    "resoconto onesto sostituito alla promessa monca",
                    _auto_mode,
                )

    return {
        "messages": [assistant_msg],
        "result": result_text,
        "provider_used": provider,
        "model_used": model,
        "latency_ms": latency_ms,
        "token_usage": token_usage or None,
        "feedback_score": None,
        "pending_tool_uses": pending_tool_uses,
        "stop_reason": stop_reason,
        "recent_tool_signatures": updated_signatures,
        "consecutive_exploration_calls": _updated_exploration_count,
        "exploration_nudge_sent": _updated_exploration_nudge_sent,
        "repeated_cmd_nudge_sent": _repeated_cmd_nudge_sent,
        "auto_escalations": escalations if loop_sig is not None else int(state.get("auto_escalations") or 0),
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "cache_creation_tokens": cache_creation_tokens,
        "cache_read_tokens": cache_read_tokens,
        "total_tokens": total_tokens,
        "total_cost_usd": total_cost_usd,
        "cache_hit_rate": cache_hit_rate,
        "temperature": temperature,
        "top_p": top_p,
        "created_at": created_at,
        # Bug-fix: incrementa iterations a ogni passaggio per il nodo executor.
        # Senza questo, il safety net `iterations >= MAX_AGENT_ITERATIONS` in
        # route_after_executor non scatta mai durante i loop
        # executor → tool_dispatch → executor o executor → executor (G1 nudge).
        # Senza l'increment, il G1 nudge poteva loop-are infinite volte con
        # iter=1 nudge=0 quando il modello produceva sempre risposte descrittive.
        "iterations": int(state.get("iterations") or 0) + 1,
        # G1: incrementa il counter nudge se è stato iniettato in questo turno
        "action_nudge_count": (
            _nudge_count + 1
            if (
                locals().get("_nudge_msg") is not None
                and not pending_tool_uses  # il nudge non ha prodotto tool calls ancora
            )
            else _nudge_count
        ),
        # G1: persisti il counter di re-execution G1 (incrementato sopra
        # all'inizio dell'executor se questa entry e' una re-entry G1).
        # Indipendente da action_nudge_count: viene alzato ANCHE quando il
        # nudge non puo' essere iniettato (es. history con tool call),
        # garantendo che il cap configurabile (agent.g1_max_nudges) scatti
        # in ogni scenario ed eviti loop infiniti.
        "g1_reroute_count": _g1_reroute_count,
        **sticky_out,
        **_thinking_payload,
        **_next_actions_payload,
    }


# ─── Nodo: tool_dispatch ─────────────────────────────────────────────────────



# ── FIX 4 (ADR 0012): budget letture allegati ──────────────────────────────
# Legge agent.attachment.session_read_budget_bytes da settings (cache 60s).
# Default 500000 = 500 KB cumulativi per sessione. Tool intercettati:


async def tool_dispatch_node(state: AgentState) -> dict[str, Any]:
    """Esegue i tool_use richiesti dall'LLM tramite ToolRunner gRPC e
    restituisce un HumanMessage con i blocchi `tool_result` corrispondenti.
    """
    pending = list(state.get("pending_tool_uses") or [])
    session_id = state.get("session_id")

    if not pending:
        return {"pending_tool_uses": [], "stop_reason": "end_turn"}

    if _tool_runner is None:
        logger.error("tool_dispatch_node: ToolRunnerClient non configurato")
        error_blocks = [
            {
                "type": "tool_result",
                "tool_use_id": b.get("id", ""),
                "content": json.dumps({"error": "tool_runner_not_configured"}),
                "is_error": True,
            }
            for b in pending
        ]
        tool_msg = HumanMessage(
            content="", additional_kwargs={"anthropic_content": error_blocks},
        )
        return {
            "messages": [tool_msg],
            "pending_tool_uses": [],
            "stop_reason": "tool_use",
        }

    if not session_id:
        logger.error("tool_dispatch_node: session_id assente, impossibile eseguire i tool")
        error_blocks = [
            {
                "type": "tool_result",
                "tool_use_id": b.get("id", ""),
                "content": json.dumps({"error": "missing_session_id"}),
                "is_error": True,
            }
            for b in pending
        ]
        tool_msg = HumanMessage(
            content="", additional_kwargs={"anthropic_content": error_blocks},
        )
        return {
            "messages": [tool_msg],
            "pending_tool_uses": [],
            "stop_reason": "tool_use",
        }

    ctx_chars = _estimate_context_chars(list(state.get("messages") or []))

    # FIX 4 (ADR 0012): budget letture allegati. Se la prossima chiamata a
    # nexus_read_attachment / nexus_read_archive_entry farebbe superare il
    # budget cumulativo della sessione, sostituisco il tool_use con un
    # tool_result sintetico che istruisce il modello a usare i tool di
    # estrazione strutturata.
    current_bytes = int(state.get("attachment_read_bytes") or 0)
    budget_total = _attachment_budget_bytes()
    synthetic_results: list[dict] = []
    pending_kept: list[dict] = []
    pending_kept_indices: list[int] = []
    # FIX D (ADR 0014): predictive context cap. Modello + system_text + history
    # corrente vengono usati per stimare se la chiamata sforerebbe ratio*window.
    _predictive_model = (
        state.get("sticky_model")
        or state.get("model_override")
        or ""
    )
    _predictive_messages = list(state.get("messages") or [])
    _predictive_system = state.get("system_text") or ""
    # M16: gate per la validazione tool-in-list nel loop sottostante.
    _M16_META_TOOLS = {"nexus_mcp_tool_search", "nexus_mcp_tool_call"}
    try:
        from brain.utils.settings_db import get_bool_setting, get_setting
        _discovery_first_on = get_bool_setting("agent.tools.discovery_first_enabled", False)
        # Whitelist dei tool SEMPRE esposti al primo turno discovery (STESSA fonte
        # DB usata da mcp-core build_tools_json: agent.tools.discovery_first_whitelist).
        # Vanno permessi dalla validazione M16 anche se non "scoperti", altrimenti
        # i tool core (list_files, read_file, ...) — pur esposti al modello —
        # verrebbero rifiutati e il modello entra in loop search->reject.
        _df_whitelist = {
            t.strip()
            for t in get_setting(
                "agent.tools.discovery_first_whitelist",
                "nexus_mcp_tool_search,nexus_mcp_tool_call",
            ).split(",")
            if t.strip()
        }
    except Exception:
        _discovery_first_on = False
        _df_whitelist = set()
    # Tool sempre ammessi dalla validazione M16 = meta di discovery + whitelist DB.
    _M16_ALLOWED = _M16_META_TOOLS | _df_whitelist
    for _i, b in enumerate(pending):
        name = b.get("name", "")
        # FIX D: predictive cap (ha priorita' sul budget allegati: se passa di qui,
        # poi viene comunque verificato anche il budget).
        _cap_msg = _predictive_cap_check(
            tool_name=str(name or ""),
            args=b.get("input", {}) or {},
            messages=_predictive_messages,
            model=str(_predictive_model or ""),
            system_text=str(_predictive_system or ""),
        )
        if _cap_msg is not None:
            synthetic_results.append({
                "type": "tool_result",
                "tool_use_id": b.get("id", ""),
                "content": _cap_msg,
                "is_error": True,
            })
            continue
        # ── M16: validazione tool-in-list (discovery-first) ──────────────────
        # Con discovery-first attivo il modello riceve solo i meta-tool
        # (search/call). Se chiama un tool NON-meta che non e' stato scoperto via
        # nexus_mcp_tool_search in questo turno (quindi non e' tra i
        # discovered_tools_next_turn iniettati), lo rifiutiamo con un feedback che
        # lo istruisce a cercarlo. Cosi' il modello e' costretto al pattern
        # search->inject native invece di chiamare a memoria tool non esposti.
        if _discovery_first_on:
            _disc_now = {
                d.get("name")
                for d in (state.get("discovered_tools_next_turn") or [])
                if isinstance(d, dict)
            }
            if name not in _M16_ALLOWED and name not in _disc_now:
                logger.info(
                    "M16: tool '%s' non scoperto/non in whitelist -> rifiutato, forzo nexus_mcp_tool_search",
                    name,
                )
                synthetic_results.append({
                    "type": "tool_result",
                    "tool_use_id": b.get("id", ""),
                    "content": json.dumps({
                        "error": (
                            f"Il tool '{name}' non e' disponibile direttamente in questo turno. "
                            f"Usa prima nexus_mcp_tool_search (query: \"{name}\") per scoprirlo, "
                            f"poi richiamalo al turno successivo."
                        )
                    }),
                    "is_error": True,
                })
                continue
        if name in _ATTACHMENT_READ_TOOLS and current_bytes >= budget_total:
            err_payload = {
                "error": (
                    f"budget letture allegati esaurito ({current_bytes} byte gia' "
                    f"letti su {budget_total} budget). Usa un tool di estrazione "
                    f"strutturata (nexus_extract_pdf_text, nexus_extract_figma_structure, "
                    f"nexus_extract_docx_text, nexus_extract_xlsx_data) oppure chiedi "
                    f"all'utente una versione testuale del file."
                ),
                "budget_bytes": budget_total,
                "already_read": current_bytes,
            }
            logger.warning(
                "tool_dispatch_node: budget letture allegati esaurito (%d/%d), tool=%s bloccato",
                current_bytes, budget_total, name,
            )
            synthetic_results.append({
                "type": "tool_result",
                "tool_use_id": b.get("id", ""),
                "content": json.dumps(err_payload, ensure_ascii=False),
                "is_error": True,
            })
        else:
            pending_kept.append(b)
            pending_kept_indices.append(_i)

    async def _run(block: dict) -> dict:
        tool_use_id = block.get("id", "")
        _t_name = block.get("name", "")
        _t_input = block.get("input", {}) or {}
        logger.info(
            "TOOL_CALL tool=%s session=%s input_keys=%s",
            _t_name,
            session_id,
            list(_t_input.keys()) if isinstance(_t_input, dict) else str(_t_input)[:80],
        )
        try:
            result = await _tool_runner.execute_tool(
                tool_name=_t_name,
                tool_input=_t_input,
                session_id=session_id,
                tool_use_id=tool_use_id,
            )
            content = _smart_truncate_lossless(
                result.result_json,
                source_kind="tool_result",
                metadata={
                    "tool_name": _t_name,
                    "session_id": session_id,
                    "thread_id": str(state.get("thread_id") or ""),
                },
            )
            return {
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
                "is_error": bool(result.is_error),
            }
        except Exception as exc:
            logger.exception("tool_dispatch_node: errore tool %s", block.get("name"))
            return {
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": json.dumps({"error": str(exc)}),
                "is_error": True,
            }

    results_kept = await asyncio.gather(*[_run(b) for b in pending_kept]) if pending_kept else []
    # Ricompongo results nell'ordine originale di pending.
    results: list[dict] = []
    _kept_iter = iter(results_kept)
    _synth_iter = iter(synthetic_results)
    for _i, b in enumerate(pending):
        if _i in pending_kept_indices:
            results.append(next(_kept_iter))
        else:
            results.append(next(_synth_iter))

    # Aggiorna byte cumulativi sommando i length dei tool_result attachment_read.
    added_bytes = 0
    for b, r in zip(pending, results):
        if b.get("name", "") in _ATTACHMENT_READ_TOOLS and not r.get("is_error"):
            added_bytes += _extract_returned_bytes(r.get("content", ""))
    new_attachment_read_bytes = current_bytes + added_bytes


    # M68: persisti gli step in agent_steps INCREMENTALMENTE durante il run.
    # Prima il batch finale era in chat_messages.rs:2350 (Rust) a fine run.
    # Ora ogni step viene scritto subito cosi' la UI lo vede via polling
    # (M67) o via refresh, senza dover aspettare la fine del run.
    try:
        import os, psycopg2  # type: ignore[import-untyped]
        from psycopg2.extras import Json as _Json  # type: ignore[import-untyped]
        _run_id = state.get("thread_id") or ""
        _dburl = os.environ.get("DATABASE_URL")
        if _run_id and _dburl:
            _step_base = int(state.get("iterations") or 0) * 1000
            _conn = psycopg2.connect(_dburl)
            try:
                with _conn.cursor() as _cur:
                    for _idx, (_block, _result) in enumerate(zip(pending, results)):
                        _t_name = _block.get("name", "")
                        _t_input = _block.get("input", {}) or {}
                        _t_result = _result.get("content", "")
                        _status = "failed" if _result.get("is_error") else "completed"
                        _cur.execute(
                            """INSERT INTO agent_steps
                               (id, run_id, step_index, tool_name, tool_input, tool_result, status, created_at)
                               VALUES (gen_random_uuid(), %s, %s, %s, %s, %s, %s, NOW())
                               ON CONFLICT DO NOTHING""",
                            (_run_id, _step_base + _idx, _t_name, _Json(_t_input), _t_result, _status),
                        )
                _conn.commit()
            finally:
                _conn.close()
    except Exception as _persist_exc:
        logger.warning("tool_dispatch_node: persistenza incrementale agent_steps fallita: %s", _persist_exc)

    new_chars = sum(len(r.get("content", "")) for r in results)
    if ctx_chars + new_chars > MAX_CONTEXT_CHARS:
        budget_per_tool = max(1500, (MAX_CONTEXT_CHARS - ctx_chars) // max(len(results), 1))
        results = [
            {
                **r,
                "content": _smart_truncate_lossless(
                    r["content"],
                    budget_per_tool,
                    source_kind="tool_result",
                    metadata={
                        "session_id": session_id,
                        "thread_id": str(state.get("thread_id") or ""),
                        "reason": "context_budget_cap",
                    },
                ),
            }
            for r in results
        ]
        logger.warning(
            "tool_dispatch_node: contesto vicino al limite (%d+%d chars), "
            "troncamento aggressivo a %d char/tool",
            ctx_chars, new_chars, budget_per_tool,
        )

    # ── PR-1: TODO reminder injection (anti-amnesia) ─────────────────────────
    # Se il plan e' attivo e siamo a una soglia di N tool use, appende
    # un blocco <system-reminder> con la checklist corrente al tool_result
    # HumanMessage. Counter resetta dopo injection.
    final_blocks = list(results)
    new_reminder_counter = int(state.get("since_last_todo_reminder", 0) or 0) + len(pending)
    if state.get("plan_phase_active"):
        from .. import todo_reminder
        # Check soglia via cfg (cache TTL 60s)
        cfg_reminder = orchestrator_config.get()
        every_n = max(1, int(cfg_reminder.get("todo_reminder_every_n_steps", 5)))
        if new_reminder_counter >= every_n:
            reminder_run_id = state.get("thread_id") or ""
            reminder_text = todo_reminder.build_reminder_text(reminder_run_id) if reminder_run_id else None
            if reminder_text:
                todo_reminder.append_reminder_block(final_blocks, reminder_text)
                # Best-effort: traccia che i todos sono stati "visti" in questa iter
                try:
                    todo_store.increment_iteration_seen(reminder_run_id)
                except Exception:
                    pass
                new_reminder_counter = 0
                logger.debug(
                    "tool_dispatch_node: TODO reminder iniettato per run_id=%s",
                    reminder_run_id,
                )

    tool_msg = HumanMessage(
        content="", additional_kwargs={"anthropic_content": final_blocks},
    )

    # Live UX: emette un meta_step per OGNI tool eseguito in questa iter.
    # Senza, durante run lunghi (npm install, build, ecc.) la chat sembra
    # ferma fino a fine run. Il generator SSE in brain/grpc_server/main.py
    # converte questi "meta_steps" in eventi `meta_step` che la UI rende
    # come fumetti progressivi (kind=tool_executed, vedi agent-meta-step-card.tsx).
    _tool_steps: list[dict] = []
    # Provider+model attuali del turno (chi ha emesso questi tool_use): noto via
    # state come `provider_used`/`model_used` (popolato da executor_node prima
    # del dispatch). Fallback su sticky_* per tool eseguiti senza un turno
    # executor immediatamente precedente. Permette al badge UI di colorare
    # ogni card tool con il provider responsabile (es. claude/anthropic vs
    # gemini/google) anche durante cascade fallback intra-run.
    _exec_provider = (
        state.get("provider_used")
        or state.get("sticky_provider")
        or state.get("provider_override")
    )
    _exec_model = (
        state.get("model_used")
        or state.get("sticky_model")
        or state.get("model_override")
    )
    for b, r in zip(pending, results):
        _ms_tool = b.get("name", "?")
        _ms_input = b.get("input", {}) or {}
        _ms_target = ""
        if isinstance(_ms_input, dict):
            for _k in ("path", "file_path", "abs_path", "command", "query", "pattern", "name", "tool_name"):
                _v = _ms_input.get(_k)
                if isinstance(_v, str) and _v:
                    _ms_target = _v if len(_v) <= 80 else (_v[:77] + "...")
                    break
        _ms_err = bool(r.get("is_error"))
        _ms_title = f"{'errore' if _ms_err else 'tool'} {_ms_tool}" + (f" — {_ms_target}" if _ms_target else "")
        _step = meta_steps.make(
            kind="tool_executed",
            title=_ms_title,
            payload={
                "tool": _ms_tool,
                "target": _ms_target,
                "is_error": _ms_err,
                "tool_use_id": b.get("id"),
                # Per-turn provider/model emittente del tool_use (UI badge).
                "provider": _exec_provider,
                "model": _exec_model,
            },
        )
        if _step:
            _tool_steps.append(_step)
            meta_steps.persist_async(state.get("thread_id"), _step)

    # ── M16: intercetta i risultati di nexus_mcp_tool_search ──────────────────
    # Estrae i tool scoperti e li passa come `discovered_tools_next_turn`, che
    # executor_node iniettera' come native nel SOLO turno successivo. Scritto
    # SEMPRE (anche []: il reducer overwrite azzera i discovered del turno prima,
    # garantendo durata esatta 1 turno).
    _discovered_next: list[dict] = []
    _max_bytes = 8192
    try:
        from brain.utils.settings_db import get_int_setting
        _max_bytes = get_int_setting("agent.tools.discovery_schema_max_bytes", 8192)
    except Exception:
        pass
    for b, r in zip(pending, results):
        if b.get("name") != "nexus_mcp_tool_search" or r.get("is_error"):
            continue
        try:
            _payload = json.loads(r.get("content") or "{}")
        except Exception:
            continue
        for _res in (_payload.get("results") or []):
            if not isinstance(_res, dict):
                continue
            _name = _res.get("tool_name") or _res.get("name")
            if not _name:
                continue
            _schema = _res.get("input_schema") or {"type": "object", "properties": {}}
            # Cap dimensione schema (evita di gonfiare il prompt del turno dopo).
            try:
                if len(json.dumps(_schema)) > _max_bytes:
                    _schema = {"type": "object", "properties": {}}
            except Exception:
                _schema = {"type": "object", "properties": {}}
            if not any(d.get("name") == _name for d in _discovered_next):
                _discovered_next.append({
                    "name": _name,
                    "description": (_res.get("description") or "")[:500],
                    "input_schema": _schema,
                })

    # Tracciamento M16: se il modello ha cercato, quanti tool sono stati estratti
    # per l'iniezione native al turno successivo.
    if any(b.get("name") == "nexus_mcp_tool_search" for b in pending):
        logger.info(
            "M16-TRACE dispatch: nexus_mcp_tool_search chiamato -> %d tool estratti per native injection",
            len(_discovered_next),
        )

    return {
        "messages": [tool_msg],
        "pending_tool_uses": [],
        "stop_reason": "tool_use",
        "since_last_todo_reminder": new_reminder_counter,
        "attachment_read_bytes": new_attachment_read_bytes,
        "meta_steps": _tool_steps,
        # M16: durata 1 turno (overwrite reducer). [] azzera i discovered precedenti.
        "discovered_tools_next_turn": _discovered_next,
    }


# ─── Routing condizionale post-executor ──────────────────────────────────────


async def reflection_node(state: AgentState) -> dict[str, Any]:
    """Esegue la self-reflection post-esecuzione e calcola il punteggio qualita'.

    Attivo solo se:
    1. reflection_enabled=true nel DB (categoria 'reflection').
    2. Il template del prompt contiene il tag <reflection> (indica v2 XML).
    3. Il campionamento probabilistico (reflection_sample_rate dal DB) lo include.

    Tutti i parametri sono letti dal DB via reflection_config (cache TTL 60s).
    Nessuna variabile d'ambiente: le modifiche admin sono attive senza rideploy.

    Flusso:
    - Legge configurazione dal DB (enabled, sample_rate, timeout_s, model, ecc.).
    - Costruisce il prompt di valutazione (rubrica statica).
    - Chiama il provider LLM con max_tokens=400 e temperature=0.
    - Parsa la risposta JSON: {score, dimensions, weaknesses, suggestions}.
    - Calcola final_reward = (1-weight)*heuristic + weight*reflection_score.
    - Persiste in nexus_agent_reflections (fire-and-forget, non blocca).
    - Se score >= reflection_reasoning_bank_min_score, bridge verso reasoning_bank.
    """
    import datetime

    # ── Legge configurazione dal DB (cache TTL 60s, zero env vars) ──────────
    cfg = reflection_config.get()
    cfg_enabled: bool = bool(cfg["reflection_enabled"])
    cfg_sample_rate: float = float(cfg["reflection_sample_rate"])
    cfg_timeout_s: float = float(cfg["reflection_timeout_s"])
    cfg_model: str = str(cfg["reflection_model"])
    cfg_reward_weight: float = float(cfg["reflection_reward_weight"])

    # ── Guard: feature flag globale (DB) ────────────────────────────────────
    if not cfg_enabled:
        logger.debug("reflection_node: disabilitato (reflection_enabled=false in DB)")
        return {"reflection_score": None, "final_reward": None}

    # ── Guard: solo prompt XML v2 con tag <reflection> ──────────────────────
    system_text = state.get("system_text") or ""
    if "<reflection>" not in system_text:
        logger.debug("reflection_node: prompt senza tag <reflection>, skip")
        return {"reflection_score": None, "final_reward": None}

    # ── Guard: sampling probabilistico (dal DB) ──────────────────────────────
    if random.random() > cfg_sample_rate:
        logger.debug(
            "reflection_node: escluso per sampling (rate=%.2f da DB)", cfg_sample_rate
        )
        return {"reflection_score": None, "final_reward": None}

    # ── Raccolta dati dallo stato ────────────────────────────────────────────
    result = state.get("result") or ""
    stop_reason = state.get("stop_reason") or "end_turn"
    iterations = int(state.get("iterations") or 0)
    profile_name = state.get("profile_name")
    provider = state.get("provider_used")
    model = state.get("model_used")
    thread_id = state.get("thread_id") or str(uuid.uuid4())
    prompt_key = ""
    prompt_version = 1

    # Recupera prompt_key dalla registry tramite il profile
    if profile_name:
        prof = profile_loader.get_profile(profile_name)
        if prof is not None:
            prompt_key = prof.prompt_key
            # La versione viene persistita come 1 di default; la precisione esatta
            # ha importanza secondaria per la reflection (serve solo come metadato
            # di tracciamento nel PromptOptimizerWorker - Fase 3).

    # Testo del task originale (primo HumanMessage)
    messages = state.get("messages", [])
    task_input = ""
    for msg in messages:
        if isinstance(msg, HumanMessage):
            task_input = msg.content if hasattr(msg, "content") else str(msg)
            break

    if not result:
        logger.debug("reflection_node: result vuoto, skip valutazione")
        return {"reflection_score": None, "final_reward": None}

    # ── Chiamata LLM reflection ──────────────────────────────────────────────
    reflection_data: dict[str, Any] | None = None
    reflection_latency_ms: int = 0
    # Il modello usato e' quello configurato in DB (reflection_model)
    reflection_model_used = cfg_model

    if _providers is not None:
        sys_prompt, user_prompt = build_reflection_prompt(task_input, result)

        # Preferisce il provider Anthropic; fallback al provider attivo del run
        reflection_provider = "anthropic" if _providers._providers.get("anthropic") else (provider or "openai")  # type: ignore[attr-defined]

        t0 = time.monotonic()
        try:
            prov = _providers._providers.get(reflection_provider)  # type: ignore[attr-defined]
            if prov is not None and hasattr(prov, "generate_completion_async"):
                full_prompt = f"{sys_prompt}\n\n{user_prompt}"
                raw_result = await asyncio.wait_for(
                    prov.generate_completion_async(
                        cfg_model,
                        full_prompt,
                        max_tokens=400,
                        temperature=0.0,
                    ),
                    timeout=cfg_timeout_s,
                )
                raw_text = raw_result.content if hasattr(raw_result, "content") else str(raw_result)
                reflection_data = parse_reflection_response(raw_text)
                if reflection_data is None:
                    logger.warning(
                        "reflection_node: parsing fallito per thread=%s", thread_id
                    )
        except asyncio.TimeoutError:
            logger.warning(
                "reflection_node: timeout (%.1fs) per thread=%s", cfg_timeout_s, thread_id
            )
        except Exception as exc:
            logger.error("reflection_node: errore chiamata LLM: %s", exc)
        finally:
            reflection_latency_ms = int((time.monotonic() - t0) * 1000)
    else:
        logger.debug("reflection_node: _providers non configurato, skip chiamata LLM")

    # ── Calcolo reward finale ────────────────────────────────────────────────
    # Reward euristico (uguale logica del learner_node)
    if stop_reason == "error":
        heuristic = 0.0
    elif iterations >= (int(state.get("iteration_budget") or 0) or MAX_AGENT_ITERATIONS):
        heuristic = 0.3
    elif result:
        heuristic = 1.0
    else:
        heuristic = 0.4

    reflection_score: float | None = None
    final_reward: float | None = None

    if reflection_data is not None:
        reflection_score = reflection_data["score"]
        heuristic_weight = round(1.0 - cfg_reward_weight, 4)
        final_reward = round(
            heuristic_weight * heuristic
            + cfg_reward_weight * reflection_score,
            4,
        )
        logger.info(
            "reflection_node: thread=%s score=%.3f heuristic=%.2f final_reward=%.4f",
            thread_id, reflection_score, heuristic, final_reward,
        )
    else:
        # Nessuna reflection disponibile: il learner usa solo euristica
        final_reward = None

    # ── Persistenza asincrona in nexus_agent_reflections ────────────────────
    if reflection_data is not None and prompt_key:
        asyncio.ensure_future(
            _persist_reflection(
                run_id=thread_id,
                prompt_key=prompt_key,
                prompt_version=prompt_version,
                reflection_data=reflection_data,
                model_used=reflection_model_used,
                latency_ms=reflection_latency_ms,
            )
        )

    # ── Bridge verso reasoning_bank (score >= soglia da DB) ─────────────────
    reasoning_bank_min = float(cfg.get("reflection_reasoning_bank_min_score", 0.85))
    if (
        reflection_data is not None
        and reflection_score is not None
        and reflection_score >= reasoning_bank_min
        and reflection_data.get("suggestions")
    ):
        from ..reasoning_bank import maybe_store_reflection_example
        asyncio.ensure_future(
            maybe_store_reflection_example(
                prompt_key=prompt_key,
                prompt_version=prompt_version,
                task_input=task_input,
                agent_output=result,
                reflection=reflection_data,
                profile_name=profile_name,
                lang=state.get("repo_lang"),
            )
        )

    return {
        "reflection_score": reflection_score,
        "reflection_dimensions": reflection_data.get("dimensions") if reflection_data else None,
        "reflection_weaknesses": reflection_data.get("weaknesses") if reflection_data else None,
        "reflection_suggestions": reflection_data.get("suggestions") if reflection_data else None,
        "final_reward": final_reward,
    }


async def _persist_reflection(
    run_id: str,
    prompt_key: str,
    prompt_version: int,
    reflection_data: dict[str, Any],
    model_used: str,
    latency_ms: int,
) -> None:
    """Persiste la reflection in nexus_agent_reflections. Fire-and-forget."""
    """Persiste la reflection in nexus_agent_reflections. Fire-and-forget."""
    try:
        from brain import db  # type: ignore[import]
        pool = db.get_pool()
        if pool is None:
            return
        import json
        async with pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO nexus_agent_reflections
                    (run_id, prompt_key, prompt_version, score, dimensions,
                     weaknesses, suggestions, model_used, latency_ms)
                VALUES ($1::uuid, $2, $3, $4, $5::jsonb, $6, $7, $8, $9)
                """,
                run_id,
                prompt_key,
                prompt_version,
                reflection_data["score"],
                json.dumps(reflection_data.get("dimensions") or {}),
                reflection_data.get("weaknesses") or [],
                reflection_data.get("suggestions") or [],
                model_used,
                latency_ms,
            )
    except Exception as exc:
        logger.error("_persist_reflection: errore DB per run_id=%s: %s", run_id, exc)


# ─── Nodo: learner ───────────────────────────────────────────────────────────

async def learner_node(state: AgentState) -> dict[str, Any]:
    """Salva l'interazione in SQLite/Qdrant e chiude il loop Q-learning.

    Oltre a persistere l'embedding per retrieval, invia un `submit_feedback`
    al `AgentRouterClient` con un reward euristico derivato dallo stato finale
    (successo = `stop_reason == "end_turn"` senza errori). Il Q-value e' cosi'
    aggiornato in nexus-orchestrator per le decisioni future.

    Cattura anche le metriche estese (token, costo, latency) nel return.
    """
    import datetime

    messages = state.get("messages", [])
    thread_id = state.get("thread_id", str(uuid.uuid4()))
    task_type = state.get("user_intent", "chat")
    behavior_mode = state.get("behavior_mode", "bilanciata")
    result = state.get("result", "")
    provider = state.get("provider_used")
    model = state.get("model_used")
    latency_ms = state.get("latency_ms")
    token_usage = state.get("token_usage")
    created_at = state.get("created_at")
    completed_at = datetime.datetime.now(datetime.timezone.utc).isoformat()

    # Recupera testo input utente
    user_input = ""
    for msg in messages:
        if isinstance(msg, HumanMessage):
            user_input = msg.content if hasattr(msg, "content") else str(msg)
            break

    qdrant_id: str | None = None

    # Stima reward preliminare (euristica base, senza reflection — disponibile subito).
    # Usata per filtrare il salvataggio Qdrant: interazioni di bassa qualita'
    # non devono inquinare il RAG (esempi falliti degradano il retrieval futuro).
    stop_reason_pre = state.get("stop_reason") or "end_turn"
    prelim_reward = (
        1.0 if stop_reason_pre == "end_turn" and result
        else 0.4 if stop_reason_pre == "end_turn"
        else 0.0 if stop_reason_pre == "error"
        else 0.3  # cap iterazioni o altro
    )

    # Leggi config learning dal DB (cache 60s). Stessa convenzione di reflection_config.
    lcfg = _get_learning_config()
    save_to_qdrant = lcfg["auto_extract"] and prelim_reward >= lcfg["min_confidence"]

    if not lcfg["auto_extract"]:
        logger.debug("learner_node: salvataggio Qdrant saltato (learning_auto_extract=false)")
    elif prelim_reward < lcfg["min_confidence"]:
        logger.debug(
            "learner_node: salvataggio Qdrant saltato (reward_prelim=%.2f < min_confidence=%.2f, stop=%s)",
            prelim_reward, lcfg["min_confidence"], stop_reason_pre,
        )

    if not result:
        logger.warning(
            "learner_node: thread=%s result vuoto (stop=%s), skip Qdrant/SQLite",
            thread_id, stop_reason_pre,
        )

    # Salva embedding in Qdrant solo se qualita' sufficiente e auto_extract abilitato
    if _retriever is not None and user_input and result and save_to_qdrant:
        interaction_text = f"Input: {user_input}\nOutput: {result}"
        qdrant_id = str(uuid.uuid4())
        payload: dict[str, Any] = {
            "thread_id": thread_id,
            "task_type": task_type,
            "behavior_mode": behavior_mode,
            "provider": provider,
            "model": model,
            "input_preview": user_input[:200],
            "output_preview": result[:200] if result else "",
        }
        stored = _retriever.store_interaction_vector(qdrant_id, interaction_text, payload)
        if not stored:
            qdrant_id = None

    # Salva in PostgreSQL (brain_learning_interactions)
    if _storage is not None and user_input:
        try:
            _storage.save_interaction(
                thread_id=thread_id,
                task_type=task_type,
                behavior_mode=behavior_mode,
                user_input=user_input,
                agent_output=result or "",
                provider=provider,
                model=model,
                latency_ms=latency_ms,
                token_usage=token_usage,
                qdrant_id=qdrant_id,
                metadata={"iterations": state.get("iterations", 1)},
            )
        except Exception as exc:
            logger.warning("learner_node: salvataggio PostgreSQL fallito thread=%s: %s", thread_id, exc)

    # ── Feedback al router Q-Learning ──────────────────────────────────────
    # Reward: usa final_reward dal reflection_node se disponibile,
    # altrimenti calcola l'euristica standard.
    #   Euristica:
    #   - end_turn pulito              -> 1.0
    #   - end_turn ma stringa vuota    -> 0.4
    #   - error                        -> 0.0
    #   - cap iterazioni raggiunto     -> 0.3
    #
    #   Reward fuso (se reflection disponibile):
    #   final_reward = 0.7 * heuristic + 0.3 * reflection_score
    profile_name = state.get("profile_name")
    stop_reason = state.get("stop_reason") or "end_turn"
    iterations = int(state.get("iterations") or 0)
    if _agent_router is not None and profile_name:
        # Calcola reward euristico (usato come fallback e per il log)
        if stop_reason == "error":
            heuristic_reward = 0.0
        elif iterations >= MAX_AGENT_ITERATIONS:
            heuristic_reward = 0.3
        elif result:
            heuristic_reward = 1.0
        else:
            heuristic_reward = 0.4

        # Usa il reward fuso dal reflection_node se disponibile
        final_reward = state.get("final_reward")
        reward = final_reward if final_reward is not None else heuristic_reward

        reflection_score = state.get("reflection_score")
        if reflection_score is not None:
            logger.info(
                "learner_node: reward fuso profile=%s heuristic=%.2f "
                "reflection=%.3f final=%.4f",
                profile_name, heuristic_reward, reflection_score, reward,
            )
        try:
            new_q = await _agent_router.submit_feedback(
                task_id=thread_id,
                task_type=task_type,
                agent_type=profile_name,
                reward=reward,
                duration_ms=int(latency_ms or 0),
                is_terminal=True,
            )
            logger.info(
                "learner_node: Q-feedback profile=%s reward=%.4f new_q=%.3f",
                profile_name, reward, new_q,
            )
        except Exception as exc:
            logger.warning("learner_node: feedback Q-learning fallito thread=%s: %s", thread_id, exc)

    logger.info(
        "learner_node: thread=%s task=%s latency=%.0fms qdrant_id=%s",
        thread_id, task_type, latency_ms or 0, qdrant_id,
    )

    return {
        "completed_at": completed_at,
    }


# ─── PR-3 Codex + Cursor: project_instructions + available_subagents blocks ─

def _build_pr3_system_blocks(state: AgentState) -> str:
    """Compone i blocchi extra da appendere al system_text:
      - <project_instructions>: contenuto di .nexus/project-instructions.md
      - <available_subagents>: catalogo kind disponibili per auto-delegation
    Best-effort: ritorna stringa vuota se nulla da iniettare.
    """
    blocks: list[str] = []
    project_id = state.get("project_id") or ""
    # 1. Project instructions (AGENTS.md / CLAUDE.md style).
    try:
        from .. import orchestrator_config, project_instructions_loader
        import os as _os
        cfg = orchestrator_config.get()
        if cfg.get("plan_phase_enabled", False) and project_id:
            url = _os.environ.get("DATABASE_URL")
            if url:
                import psycopg2  # type: ignore[import-untyped]
                with psycopg2.connect(url) as conn:
                    proj_root = None
                    try:
                        with conn.cursor() as cur:
                            cur.execute(
                                "SELECT absolute_path FROM workspaces WHERE project_id=%s AND is_primary=true LIMIT 1",
                                (project_id,),
                            )
                            row = cur.fetchone()
                            proj_root = row[0] if row else None
                    except Exception:
                        proj_root = None
                    instr = project_instructions_loader.load_project_instructions(
                        conn, project_id, proj_root,
                    )
                    if instr:
                        blocks.append(instr)
    except Exception:
        pass
    # 2. Available subagents (Cursor pattern auto-delegation).
    try:
        from .. import orchestrator_config, subagent_store, prompt_registry, prompt_renderer
        cfg = orchestrator_config.get()
        if cfg.get("subagents_enabled", False) and cfg.get("auto_delegation_enabled", True):
            proj_root = None
            url = __import__("os").environ.get("DATABASE_URL", "")
            if url and project_id:
                try:
                    import psycopg2  # type: ignore[import-untyped]
                    with psycopg2.connect(url) as conn:
                        with conn.cursor() as cur:
                            cur.execute(
                                "SELECT absolute_path FROM workspaces WHERE project_id=%s AND is_primary=true LIMIT 1",
                                (project_id,),
                            )
                            row = cur.fetchone()
                            proj_root = row[0] if row else None
                except Exception:
                    pass
            kinds = subagent_store.list_enabled_kinds(proj_root)
            if kinds:
                wl = (cfg.get("subagent_kinds_whitelist") or "").split(",")
                wl = [w.strip() for w in wl if w.strip()]
                if wl:
                    kinds = [k for k in kinds if k.get("kind") in wl]
            if kinds:
                lines = []
                for k in kinds:
                    bg = " (background)" if k.get("is_background") else ""
                    lines.append(f"- {k['kind']}{bg}: {k.get('description','')}")
                subagents_block = "\n".join(lines)
                tpl = prompt_registry.get_prompt("system.available_subagents_block") or ""
                if tpl:
                    rendered = prompt_renderer.render(tpl, {
                        "subagents_block": subagents_block,
                        "max_parallel": cfg.get("max_parallel_subagents", 3),
                    })
                    blocks.append(rendered)
                else:
                    blocks.append(f"<available_subagents>\n{subagents_block}\n</available_subagents>")
    except Exception:
        pass
    return "\n\n".join(b for b in blocks if b)


# ─── Funzione di routing condizionale ────────────────────────────────────────
