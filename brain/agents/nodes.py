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
import random
import time
import uuid
from typing import TYPE_CHECKING, Any

from langchain_core.messages import AIMessage, HumanMessage

from . import profile_loader, prompt_registry, prompt_renderer, reflection_config
from .reflection_rubric import build_reflection_prompt, parse_reflection_response
from .state import AgentState

if TYPE_CHECKING:
    from brain.embeddings import EmbeddingService
    from brain.grpc_clients.agent_router_client import AgentRouterClient
    from brain.grpc_clients.tool_runner_client import ToolRunnerClient
    from brain.memory.retrieval import InteractionRetriever
    from brain.memory.storage import LocalLearningStorage
    from brain.providers import ProviderRegistry
    from brain.router import SemanticRouter

logger = logging.getLogger(__name__)

# Cap iterazioni agent loop (richiesta executor -> tool_dispatch -> executor ...).
MAX_AGENT_ITERATIONS = 25

# ── Lookup prezzi modelli da ai_price_catalog ──────────────────────────────
_PRICE_CACHE: dict[str, tuple[float, float]] = {}
_PRICE_CACHE_TS: dict[str, float] = {}
_PRICE_TTL_S = 300.0


def _lookup_price(provider: str, model: str) -> tuple[float, float]:
    """Ritorna (input_cost_per_mtok, output_cost_per_mtok) da ai_price_catalog.
    Cache 5min. Ritorna (0,0) se modello non trovato (niente errore bloccante)."""
    import os
    key = f"{provider}|{model}"
    now = time.monotonic()
    if key in _PRICE_CACHE and (now - _PRICE_CACHE_TS.get(key, 0)) < _PRICE_TTL_S:
        return _PRICE_CACHE[key]
    try:
        import psycopg2
        db_url = os.environ.get(
            "DATABASE_URL",
            "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable",
        )
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT input_cost_per_million_tokens, output_cost_per_million_tokens "
                    "FROM ai_price_catalog "
                    "WHERE provider = %s AND model = %s AND is_enabled = TRUE "
                    "ORDER BY effective_from DESC LIMIT 1",
                    (provider, model),
                )
                row = cur.fetchone()
        if row:
            result = (float(row[0]), float(row[1]))
        else:
            result = (0.0, 0.0)
    except Exception as e:
        logger.warning("_lookup_price(%s/%s) fallito: %s", provider, model, e)
        result = (0.0, 0.0)
    _PRICE_CACHE[key] = result
    _PRICE_CACHE_TS[key] = now
    return result

# ── Limiti output tool per gestione contesto ────────────────────────────────
# Tronca risultati tool singoli troppo grandi (20% testa + 80% coda)
MAX_TOOL_RESULT_CHARS = 6000
# Budget totale contesto: oltre questo soglia i tool result vecchi vengono compressi
MAX_CONTEXT_CHARS = 400_000


def _smart_truncate(text: str, max_chars: int = MAX_TOOL_RESULT_CHARS) -> str:
    """Tronca preservando testa (20%) e coda (80%) per mantenere errori finali."""
    if len(text) <= max_chars:
        return text
    head_size = max_chars // 5
    tail_size = max_chars - head_size - 80
    return (
        text[:head_size]
        + f"\n\n[... TRONCATO: {len(text) - head_size - tail_size} caratteri omessi ...]\n\n"
        + text[-tail_size:]
    )


def _estimate_context_chars(messages: list) -> int:
    """Stima il numero totale di caratteri nel contesto messaggi."""
    total = 0
    for m in messages:
        if hasattr(m, "content") and isinstance(m.content, str):
            total += len(m.content)
        kwargs = getattr(m, "additional_kwargs", {}) or {}
        for block in kwargs.get("anthropic_content", []):
            if isinstance(block, dict):
                c = block.get("content", "")
                total += len(c) if isinstance(c, str) else 0
    return total

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

    database_url = os.environ.get("DATABASE_URL", "")
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
) -> None:
    """Inietta i servizi globali nei nodi. Chiamato da create_agent_graph()."""
    global _providers, _router, _embeddings, _storage, _retriever
    global _tool_runner, _agent_router
    _providers = providers
    _router = router
    _embeddings = embeddings
    _storage = storage
    _retriever = retriever
    _tool_runner = tool_runner
    _agent_router = agent_router


# ─── RAG helper (BP7) ───────────────────────────────────────────────────────

# Intent per i quali il RAG inline e' utile: task che operano su codice/repo
# e tipicamente beneficiano del ricordo di task simili passati.
_RAG_INTENTS = {"code", "code_edit", "code_read", "refactor", "analyze",
                "fix", "implement", "debug", "review"}

# Soglia minima di similarita' per includere un'interazione nel contesto.
# Sotto questa soglia il match non e' significativo e introdurrebbe rumore.
_RAG_MIN_SCORE = 0.5

# Numero massimo di interazioni recuperate per turno.
_RAG_TOP_K = 5

# Limite caratteri per ogni snippet incluso (per non gonfiare il system prompt).
_RAG_SNIPPET_MAX_CHARS = 400


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
    try:
        hits = _retriever.get_similar_interactions(
            query_text=query_text, task_type=None, limit=_RAG_TOP_K,
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
        if len(text) > _RAG_SNIPPET_MAX_CHARS:
            text = text[: _RAG_SNIPPET_MAX_CHARS - 3] + "..."
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


# ─── Nodo: router ────────────────────────────────────────────────────────────

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

    if _router is not None:
        classification = _router.classify_intent(str(text))
        intent = classification.get("intent", "chat")
    else:
        intent = "chat"

    behavior_mode = state.get("behavior_mode", "bilanciata")

    # Boost token_budget per intent complessi che richiedono piu' output
    # (generazione documenti, analisi codice, fix estesi).
    COMPLEX_INTENTS = {"doc_generate", "analyze", "fix", "refactor", "implement"}
    text_lower = str(text).lower()
    is_complex = intent in COMPLEX_INTENTS or any(
        kw in text_lower for kw in ("genera", "document", "analisi tecnica", "implementa")
    )
    if is_complex:
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
            if not sel.is_empty:
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
        except Exception as exc:
            logger.debug("router_node: agent_router select fallito: %s", exc)

    if profile is None:
        profile = profile_loader.route_profile_for_intent(intent)

    updates: dict[str, Any] = {
        "user_intent": intent,
        "task_type": intent,
        "behavior_mode": behavior_mode,
        "token_budget": token_budget,
        "iterations": state.get("iterations", 0) + 1,
    }

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
                updates["system_text"] = rendered
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

    return updates


# ─── Nodo: executor ──────────────────────────────────────────────────────────

def _dedup_tool_results(messages: list[Any]) -> list[Any]:
    """Dedup semantico dei tool_result duplicati (BP11 piano riduzione token).

    Quando lo stesso file viene letto piu' volte (frequente in sessioni
    edit-heavy: read_file + edit + read_file di nuovo per verifica), la
    history accumula copie del contenuto. Manteniamo solo l'ULTIMA copia
    e sostituiamo le precedenti con un marker compatto che cita il msg
    successivo.

    Hash key: SHA-1 di (content_normalizzato[:256]). Non includiamo il
    tool_use_id (cambia ad ogni invocazione) perche' vogliamo proprio
    intercettare il caso in cui invocazioni diverse producono lo stesso
    output.

    NB: il tool_use_id del blocco tool_result resta invariato -- Anthropic
    valida l'accoppiamento tool_use<->tool_result solo sull'id, non sul
    content. Quindi modificare il content e' sicuro.
    """
    import hashlib

    # Prima passata: mappa hash -> indice messaggio dell'ultima occorrenza.
    # last_indices[h] = (msg_idx, block_idx)
    last_indices: dict[str, tuple[int, int]] = {}
    for mi, m in enumerate(messages):
        extra = getattr(m, "additional_kwargs", {}) or {}
        blocks = extra.get("anthropic_content")
        if not isinstance(blocks, list):
            continue
        for bi, block in enumerate(blocks):
            if not isinstance(block, dict) or block.get("type") != "tool_result":
                continue
            content = block.get("content", "")
            if isinstance(content, list):
                content = " ".join(
                    str(b.get("text", "")) for b in content
                    if isinstance(b, dict) and b.get("type") == "text"
                )
            if not isinstance(content, str) or len(content) < 200:
                continue  # skip content piccoli: dedup non porta beneficio
            normalized = content.strip()[:256]
            h = hashlib.sha1(normalized.encode("utf-8", errors="ignore")).hexdigest()[:16]
            last_indices[h] = (mi, bi)

    # Seconda passata: sostituisci le occorrenze NON ultime con marker.
    deduped_count = 0
    new_messages = []
    for mi, m in enumerate(messages):
        extra = getattr(m, "additional_kwargs", {}) or {}
        blocks = extra.get("anthropic_content")
        if not isinstance(blocks, list):
            new_messages.append(m)
            continue
        changed = False
        new_blocks = []
        for bi, block in enumerate(blocks):
            if not isinstance(block, dict) or block.get("type") != "tool_result":
                new_blocks.append(block)
                continue
            content = block.get("content", "")
            if isinstance(content, list):
                serialized = " ".join(
                    str(b.get("text", "")) for b in content
                    if isinstance(b, dict) and b.get("type") == "text"
                )
            elif isinstance(content, str):
                serialized = content
            else:
                new_blocks.append(block)
                continue
            if not serialized or len(serialized) < 200:
                new_blocks.append(block)
                continue
            normalized = serialized.strip()[:256]
            h = hashlib.sha1(normalized.encode("utf-8", errors="ignore")).hexdigest()[:16]
            last_mi, last_bi = last_indices.get(h, (mi, bi))
            if (mi, bi) != (last_mi, last_bi):
                # Non e' l'ultima occorrenza: sostituisci con marker.
                new_blocks.append({
                    **block,
                    "content": f"[deduped: contenuto identico al tool_result piu' recente in msg #{last_mi}]",
                })
                changed = True
                deduped_count += 1
            else:
                new_blocks.append(block)
        if changed:
            new_msg = HumanMessage(
                content=getattr(m, "content", ""),
                additional_kwargs={"anthropic_content": new_blocks},
            )
            new_messages.append(new_msg)
        else:
            new_messages.append(m)

    if deduped_count > 0:
        logger.info("dedup_tool_results: rimosse %d copie duplicate", deduped_count)
    return new_messages


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
                        "content": content[:kept] + f"\n[... compresso: {len(content)} char originali ...]",
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
                    "content": content[:kept] + f"\n[... compresso: {len(content)} char originali ...]",
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


def _langchain_to_anthropic_messages(messages: list[Any]) -> list[dict]:
    """Converte la lista di BaseMessage LangChain in messaggi Anthropic-compatible.

    Gestisce content=str e content=list[dict] (blocchi tool_use/tool_result).
    """
    out: list[dict] = []
    for m in messages:
        role = getattr(m, "type", None)
        if role == "human":
            anth_role = "user"
        elif role == "ai":
            anth_role = "assistant"
        elif role == "tool":
            anth_role = "user"
        else:
            anth_role = "user"
        content = getattr(m, "content", "")
        extra = getattr(m, "additional_kwargs", {}) or {}
        structured = extra.get("anthropic_content")
        if structured is not None:
            out.append({"role": anth_role, "content": structured})
        else:
            out.append({"role": anth_role, "content": content})
    return out


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

    # ── Forced text response (anti-loop tool-only) ────────────────────────
    # Se il loop ha gia' consumato la maggior parte delle iterazioni concesse
    # (>= MAX_AGENT_ITERATIONS - 5) e il modello sta ancora facendo tool calls
    # (stop_reason precedente era "tool_use"), svuotiamo temporaneamente i tool
    # per forzare una risposta testuale nell'ultima finestra di iterazioni.
    # Questo previene loop in cui modelli small (es. Mistral) fanno tool calls
    # continue senza mai produrre testo, consumando tutte le iterazioni senza
    # lasciare una risposta utile all'utente.
    _current_iterations = int(state.get("iterations") or 0)
    _FORCED_TEXT_THRESHOLD = MAX_AGENT_ITERATIONS - 5
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

    # Override esplicito batte il routing semantico.
    provider = state.get("provider_override")
    model = state.get("model_override")
    if not provider or not model:
        if _router is not None:
            # Passa anche il message originale: il router lo usa per detection
            # task rischiosi (override automatico a behavior_mode "approfondita"
            # se rileva verbi distruttivi: rm -rf, drop table, docker prune, ecc.)
            decision = _router.route_model(intent, token_budget, behavior_mode, message=str(text))
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

    logger.info(
        "executor_node: provider=%s model=%s intent=%s tools=%d",
        provider, model, intent, len(tools_json),
    )

    start_ms = time.monotonic() * 1000
    created_at = datetime.datetime.utcnow().isoformat() + "Z"
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
        # ── Compressione escalante (BP4 esteso) ─────────────────────────────
        # La compressione si attiva se:
        # a) il contesto supera il 50% di MAX_CONTEXT_CHARS (soglia classica), oppure
        # b) le iterazioni salgono (loop): keep_recent e max_content_chars decrescono
        #    progressivamente per limitare l'accumulo di tool_result nella history.
        #
        # Calcolo keep_recent escalante:
        #   iter < 8  → keep_recent=6, max_content_chars=500 (default)
        #   iter 8-11 → keep_recent=5, max_content_chars=350
        #   iter 12-15→ keep_recent=4, max_content_chars=250
        #   iter 16-19→ keep_recent=3, max_content_chars=200
        #   iter >= 20→ keep_recent=2, max_content_chars=150
        _compress_iter = _current_iterations  # _current_iterations letto sopra
        if _compress_iter >= 8:
            _keep_recent = max(2, 6 - (_compress_iter - 8) // 4)
            _max_chars = max(150, 500 - (_compress_iter - 8) * 25)
            messages = _compress_old_tool_results(
                messages, keep_recent=_keep_recent, max_content_chars=_max_chars,
            )
            new_size = _estimate_context_chars(messages)
            logger.info(
                "executor_node: compressione escalante iter=%d keep_recent=%d "
                "max_content_chars=%d: %d -> %d char",
                _compress_iter, _keep_recent, _max_chars, ctx_size, new_size,
            )
            ctx_size = new_size
        elif ctx_size > MAX_CONTEXT_CHARS // 2:
            messages = _compress_old_tool_results(messages, keep_recent=6)
            new_size = _estimate_context_chars(messages)
            logger.info(
                "executor_node: contesto compresso %d -> %d char",
                ctx_size, new_size,
            )
        anth_messages = _langchain_to_anthropic_messages(messages)
        try:
            p = _providers._providers.get(provider)  # type: ignore[attr-defined]
            if p is None or not hasattr(p, "generate_agent_turn"):
                raise RuntimeError(f"provider {provider} non supporta agent_turn")
            # max_tokens dinamico: almeno 8192 per turni con tool, capped a 16384.
            # Il token_budget dallo state (stimato dal router_node) viene usato
            # come base, ma per agent turn con tool serve molto piu' spazio
            # (tool_use JSON e' verboso, content_json per documenti puo' essere enorme).
            effective_max_tokens = max(8192, min(token_budget * 4, 16384))
            prov_result = await p.generate_agent_turn(
                model, anth_messages, tools_json,
                max_tokens=effective_max_tokens, system_text=system_text,
            )
            result_text = prov_result.content or ""
            meta = prov_result.metadata or {}
            stop_reason = meta.get("stop_reason") or "end_turn"
            pending_tool_uses = list(meta.get("tool_use_blocks") or [])
            assistant_content = meta.get("assistant_content")
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

            # Registra l'usage di questo turno nel billing ledger (B1 fix).
            try:
                from brain.providers.registry import record_agent_turn_usage
                record_agent_turn_usage(
                    provider=provider,
                    model=model,
                    prompt_tokens=prompt_tokens,
                    completion_tokens=completion_tokens,
                    cache_read_tokens=cache_read_tokens,
                    cache_creation_tokens=cache_creation_tokens,
                    iteration=_current_iterations,
                    run_id=str(state.get("thread_id") or ""),
                )
            except Exception as billing_exc:
                logger.warning("executor_node: billing ledger fallito iter=%d: %s", _current_iterations, billing_exc)

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

                # === Tier 1: catena intra-provider ===
                try:
                    import psycopg2  # type: ignore[import]
                    import os as _os
                    _db_url = _os.environ.get(
                        "DATABASE_URL",
                        "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable",
                    )
                    with psycopg2.connect(_db_url) as _conn:
                        with _conn.cursor() as _cur:
                            _cur.execute(
                                "SELECT escalation_model FROM nexus_model_escalation_chain "
                                "WHERE provider = %s AND base_model = %s AND is_active = TRUE "
                                "ORDER BY escalation_position ASC LIMIT %s",
                                (provider, model, escalations + 1),
                            )
                            _rows = _cur.fetchall()
                    if _rows and len(_rows) > escalations:
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

    # Calcola cache hit rate
    total_tokens = prompt_tokens + completion_tokens + cache_creation_tokens + cache_read_tokens
    cache_hit_rate = cache_read_tokens / total_tokens if total_tokens > 0 else 0.0

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
    }


# ─── Nodo: tool_dispatch ─────────────────────────────────────────────────────

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
            content = _smart_truncate(result.result_json)
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

    results = await asyncio.gather(*[_run(b) for b in pending])

    new_chars = sum(len(r.get("content", "")) for r in results)
    if ctx_chars + new_chars > MAX_CONTEXT_CHARS:
        budget_per_tool = max(1500, (MAX_CONTEXT_CHARS - ctx_chars) // max(len(results), 1))
        results = [
            {**r, "content": _smart_truncate(r["content"], budget_per_tool)}
            for r in results
        ]
        logger.warning(
            "tool_dispatch_node: contesto vicino al limite (%d+%d chars), "
            "troncamento aggressivo a %d char/tool",
            ctx_chars, new_chars, budget_per_tool,
        )

    tool_msg = HumanMessage(
        content="", additional_kwargs={"anthropic_content": list(results)},
    )

    return {
        "messages": [tool_msg],
        "pending_tool_uses": [],
        "stop_reason": "tool_use",
    }


# ─── Routing condizionale post-executor ──────────────────────────────────────

def route_after_executor(state: AgentState) -> str:
    """Decide se iterare (tool_dispatch) o chiudere (learner).

    Safety cap: superato MAX_AGENT_ITERATIONS forza learner per evitare
    loop infiniti.
    Loop detection: stop_reason="loop_detected" forza chiusura immediata.
    """
    iterations = int(state.get("iterations") or 0)
    stop_reason = state.get("stop_reason")
    pending = state.get("pending_tool_uses") or []
    if stop_reason == "loop_detected":
        logger.warning("route_after_executor: loop detected, chiusura forzata")
        return "learner"
    if iterations >= MAX_AGENT_ITERATIONS:
        logger.warning("route_after_executor: cap iterazioni raggiunto (%d)", iterations)
        return "learner"
    if stop_reason == "tool_use" and pending:
        return "tool_dispatch"
    return "learner"


# ─── Nodo: reflection ────────────────────────────────────────────────────────

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
    elif iterations >= MAX_AGENT_ITERATIONS:
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
        from .reasoning_bank import maybe_store_reflection_example
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
    completed_at = datetime.datetime.utcnow().isoformat() + "Z"

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

    # Salva in SQLite
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
            logger.warning("learner_node: salvataggio SQLite fallito thread=%s: %s", thread_id, exc)

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


# ─── Funzione di routing condizionale ────────────────────────────────────────

def route_by_task_type(state: AgentState) -> str:
    """Routing condizionale: mappa task_type al nodo executor."""
    # Tutti i task_type validi vanno verso executor
    return "executor"
