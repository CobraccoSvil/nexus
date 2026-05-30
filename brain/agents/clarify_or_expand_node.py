"""clarify_or_expand_node (Fase 2 — pubblicazione step + normalizzazione).

Nodo condizionale inserito tra `router_node` e il routing legacy/planner.
Si attiva SOLO quando il classifier intent segnala bassa fiducia, per evitare
overhead inutile sui prompt chiari (~99% dei casi).

Due possibili output mutuamente esclusivi:
  - mode="ask"     → emette un meta_step `clarify` con la domanda all'utente
                     e setta `pending_clarify=True`. Il graph va a END e il
                     turno si ferma. Il frontend mostra una card non
                     collassabile con la domanda; il prossimo messaggio
                     dell'utente proseguira' la conversazione.
  - mode="expand"  → popola `expanded_query` nello state per arricchire il
                     retrieve RAG. NON sostituisce il prompt originale: il
                     messaggio utente passa intatto al modello principale.

Feature flag: `orchestrator.clarify.enabled` (default true). Soglie:
  - `orchestrator.clarify.confidence_threshold` (default 0.6)
  - `orchestrator.clarify.require_llm_classifier` (default false — usa anche
    il fallback heuristico).

Resilienza: ogni errore (modello non risolvibile, prompt mancante, LLM call
fallita, output non parsabile) ricade in no-op senza bloccare il run.
"""
from __future__ import annotations

import json
import logging
import os
from typing import Any

from . import meta_steps, prompt_registry
from .state import AgentState

logger = logging.getLogger(__name__)

# Iniettati da configure(); riusiamo gli stessi singletoni di planner_node.
_providers: Any = None
_routing_client: Any = None

# Endpoint interno no-auth gia' usato altrove: riuso del servizio di ricerca
# vettoriale esistente (condizione di integrazione, niente client nuovo).
_MCP_CORE_INTERNAL_URL = os.environ.get("MCP_CORE_INTERNAL_URL", "http://localhost:4000")


def configure(providers: Any, routing_client: Any) -> None:
    """Iniettato dal grpc_server in startup, in parallelo a planner_node."""
    global _providers, _routing_client
    _providers = providers
    _routing_client = routing_client


def _load_config() -> dict[str, Any]:
    """Carica config clarify dalla tabella settings (cache 60s in
    orchestrator_config). Restituisce default se DB irraggiungibile."""
    defaults = {
        "enabled": True,
        "confidence_threshold": 0.6,
        "require_llm_classifier": False,
        "prompt_key": "agent.clarify.base",
        "max_question_chars": 280,
        # Cluster 4: lookup decisione gia' presa + conferma decisioni di prodotto.
        "decision_lookup_enabled": False,
        "decision_min_score": 0.7,
        "decision_topk": 5,
        "confirm_irreversible_in_auto": False,
    }
    url = os.environ.get("DATABASE_URL", "")
    if not url:
        return defaults
    try:
        import psycopg2  # type: ignore[import-untyped]
        with psycopg2.connect(url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT key, value FROM settings "
                    "WHERE category = 'orchestrator' AND key LIKE 'clarify.%'"
                )
                for key, value in cur.fetchall():
                    short = key.replace("clarify.", "")
                    if short == "enabled":
                        defaults["enabled"] = str(value).lower() not in ("false", "0", "off")
                    elif short == "confidence_threshold":
                        try:
                            defaults["confidence_threshold"] = float(value)
                        except (TypeError, ValueError):
                            pass
                    elif short == "require_llm_classifier":
                        defaults["require_llm_classifier"] = str(value).lower() not in ("false", "0", "off")
                    elif short == "prompt_key":
                        defaults["prompt_key"] = str(value)
                    elif short == "max_question_chars":
                        try:
                            defaults["max_question_chars"] = int(value)
                        except (TypeError, ValueError):
                            pass
                    elif short == "decision_lookup_enabled":
                        defaults["decision_lookup_enabled"] = str(value).lower() not in ("false", "0", "off")
                    elif short == "decision_min_score":
                        try:
                            defaults["decision_min_score"] = float(value)
                        except (TypeError, ValueError):
                            pass
                    elif short == "decision_topk":
                        try:
                            defaults["decision_topk"] = int(value)
                        except (TypeError, ValueError):
                            pass
                    elif short == "confirm_irreversible_in_auto":
                        defaults["confirm_irreversible_in_auto"] = str(value).lower() not in ("false", "0", "off")
    except Exception as exc:
        logger.debug("clarify_or_expand: _load_config fallback (%s)", exc)
    return defaults


def _last_user_message(state: AgentState) -> str:
    for m in reversed(state.get("messages", []) or []):
        if getattr(m, "type", None) in ("human", "user"):
            content = getattr(m, "content", "") or ""
            return content if isinstance(content, str) else str(content)
    return ""


def _lookup_existing_decision(state: AgentState, user_msg: str, cfg: dict) -> dict | None:
    """Cluster 4: cerca se la decisione e' GIA' stata presa (note intent=decision)
    via il servizio di ricerca vettoriale esistente. Ritorna la nota se il best
    match supera la soglia, altrimenti None. Best-effort, mai solleva.
    """
    if not cfg.get("decision_lookup_enabled"):
        return None
    project_id = str(state.get("project_id") or "").strip()
    if not project_id or len(user_msg) < 10:
        return None
    try:
        import requests  # noqa: PLC0415
        resp = requests.post(
            f"{_MCP_CORE_INTERNAL_URL}/api/internal/knowledge/search",
            json={
                "project_id": project_id,
                "query": user_msg,
                "top_k": int(cfg.get("decision_topk", 5)),
                "min_score": float(cfg.get("decision_min_score", 0.7)),
            },
            timeout=5,
        )
        if resp.status_code != 200:
            return None
        results = resp.json().get("results", []) or []
    except Exception as exc:
        logger.debug("clarify_or_expand: decision lookup fallito (%s)", exc)
        return None
    # Solo note di tipo decision sopra soglia.
    min_score = float(cfg.get("decision_min_score", 0.7))
    decisions = [
        r for r in results
        if (r.get("intent") or "") == "decision" and float(r.get("score") or 0) >= min_score
    ]
    return decisions[0] if decisions else None


async def clarify_or_expand_node(state: AgentState) -> dict[str, Any]:
    """Decide se chiedere chiarimento o arricchire la query per il retrieve.

    No-op (ritorna {}) quando:
      - flag disabilitato
      - intent gia' chiaro (confidence >= soglia)
      - prompt template mancante
      - servizi non configurati
    """
    cfg = _load_config()
    if not cfg["enabled"]:
        logger.info("clarify_or_expand: disabilitato via settings, skip")
        return {}

    user_msg_preview = _last_user_message(state).strip()

    # ── Cluster 4: la decisione e' GIA' stata presa? (RAG, gated) ────────────
    # Cerca decisioni passate; se ne trova una con score alto, la applica
    # (meta_step resolved_from_memory) e prosegue SENZA chiedere.
    existing_decision = _lookup_existing_decision(state, user_msg_preview, cfg)
    if existing_decision is not None:
        run_id = state.get("thread_id")
        ms = meta_steps.make(
            kind="clarify",
            title="Decisione recuperata dalla memoria",
            payload={
                "mode": "resolved_from_memory",
                "source_note_id": existing_decision.get("note_id"),
                "source_title": existing_decision.get("title"),
                "score": existing_decision.get("score"),
                "intent": state.get("user_intent"),
            },
        )
        updates: dict[str, Any] = {}
        if ms:
            updates["meta_steps"] = [ms]
            meta_steps.persist_async(run_id, ms)
        logger.info(
            "clarify_or_expand: decisione recuperata dalla memoria (%s, score=%.2f), no domanda",
            existing_decision.get("title"), float(existing_decision.get("score") or 0),
        )
        return updates

    # ── Short-circuit autonomia ──────────────────────────────────────────────
    # In automatico l'agente procede autonomo. ECCEZIONE Cluster 4: se
    # confirm_irreversible_in_auto e' ON, NON short-circuit-iamo: classifichiamo
    # la richiesta e chiediamo conferma SOLO se e' una decisione di prodotto/
    # irreversibile (il resto prosegue autonomo).
    automation = (state.get("automation_mode") or "").strip().lower()
    is_auto = automation in ("automatic", "automatico", "auto", "continuous", "continuo")
    force_classify = bool(is_auto and cfg.get("confirm_irreversible_in_auto"))
    if is_auto and not force_classify:
        logger.info(
            "clarify_or_expand: skip (automation_mode=%s, agente autonomo)",
            automation,
        )
        return {}
    confidence = float(state.get("intent_confidence") or 1.0)
    logger.info(
        "clarify_or_expand: entrata run_id=%s confidence=%.2f threshold=%.2f",
        state.get("thread_id"), confidence, float(cfg["confidence_threshold"]),
    )
    threshold = float(cfg["confidence_threshold"])

    # Trigger secondario euristico: il classifier keyword di Nexus restituisce
    # confidence >= 0.75 anche per match deboli, sotto la soglia 0.6 non si
    # scenderebbe mai. Mentre attendiamo il classifier embedding completo,
    # consideriamo "ambigui" i messaggi corti che contengono fillers vaghi.
    _AMBIG_TOKENS = (
        "quella cosa", "qualcosa", "boh", "non so", "aiuto",
        "fai tu", "fai un po'", "non saprei", "fai quello",
        "puoi farmi", "puoi farci",
    )
    is_heuristic_ambiguous = (
        len(user_msg_preview) < 40
        and any(tok in user_msg_preview.lower() for tok in _AMBIG_TOKENS)
    )
    # force_classify (Cluster 4): in automatico con conferma decisioni di
    # prodotto attiva, classifichiamo SEMPRE per intercettare le decisioni
    # irreversibili, anche con confidence alta.
    if confidence >= threshold and not is_heuristic_ambiguous and not force_classify:
        logger.info(
            "clarify_or_expand: skip (confidence %.2f >= %.2f, no heuristic match)",
            confidence, threshold,
        )
        return {}
    logger.info(
        "clarify_or_expand: trigger (confidence=%.2f heuristic=%s msg=%r)",
        confidence, is_heuristic_ambiguous, user_msg_preview[:60],
    )

    if _providers is None or _routing_client is None:
        logger.info(
            "clarify_or_expand: servizi non configurati (_providers=%s _routing_client=%s), skip",
            _providers is not None, _routing_client is not None,
        )
        return {}

    # Fallback heuristico se LLM classifier disabilitato e richiesto via flag.
    if cfg["require_llm_classifier"]:
        if os.environ.get("NEXUS_LLM_CLASSIFIER_ENABLED", "false").lower() not in ("true", "1", "yes"):
            return {}

    user_msg = user_msg_preview
    if not user_msg or len(user_msg) < 3:
        return {}

    # Risolvi modello cheap via nexus_purpose_model.clarify_expand.
    try:
        decision = _routing_client.purpose_model(purpose="clarify_expand")
        provider, model = decision.provider, decision.model
        logger.info("clarify_or_expand: purpose_model -> %s/%s", provider, model)
    except Exception as exc:
        logger.warning("clarify_or_expand: purpose_model fallito (%s), skip", exc)
        return {}

    system_text = prompt_registry.get_prompt(cfg["prompt_key"]) or ""
    if not system_text:
        logger.debug("clarify_or_expand: prompt '%s' non trovato, skip", cfg["prompt_key"])
        return {}

    # Tool con schema strutturato per garantire un JSON parsabile.
    tools_json = [{
        "name": "clarify_or_expand",
        "description": "Decidi se serve un chiarimento all'utente o un'espansione della query per il retrieve.",
        "input_schema": {
            "type": "object",
            "properties": {
                "mode": {"type": "string", "enum": ["ask", "expand", "skip"]},
                "question": {"type": "string"},
                "expanded_query": {"type": "string"},
                "rationale": {"type": "string"},
                "category": {
                    "type": "string",
                    "enum": ["technical", "product", "irreversible"],
                    "description": "Tipo di decisione: technical (scelta implementativa), product (scelta di prodotto/UX/business), irreversible (azione difficile da annullare).",
                },
                "reversible": {"type": "boolean", "description": "True se la decisione e' facilmente reversibile."},
            },
            "required": ["mode"],
        },
    }]

    anth_messages = [{"role": "user", "content": user_msg}]
    try:
        import asyncio as _aio
        result = await _aio.to_thread(
            _providers.generate_agent_turn_sync,
            provider, model, anth_messages, tools_json,
            max_tokens=512, system_text=system_text,
        )
    except Exception as exc:
        logger.warning("clarify_or_expand: LLM call fallita (%s), skip", exc)
        return {}

    meta = result.metadata or {}
    blocks = meta.get("tool_use_blocks") or []
    block = next((b for b in blocks if b.get("name") == "clarify_or_expand"), None)
    if block is None:
        logger.info(
            "clarify_or_expand: tool_use 'clarify_or_expand' non emesso (blocks=%d, content=%r), skip",
            len(blocks), str(result.content or "")[:120],
        )
        return {}
    inp = block.get("input") or {}
    mode = str(inp.get("mode") or "skip").lower()
    category = str(inp.get("category") or "technical").lower()
    reversible = inp.get("reversible")
    reversible = True if reversible is None else bool(reversible)
    logger.info("clarify_or_expand: LLM ha scelto mode=%s category=%s reversible=%s", mode, category, reversible)

    run_id = state.get("thread_id")

    # Cluster 4: in modalita' automatica (force_classify) NON interrompiamo per
    # decisioni tecniche/reversibili — l'agente procede autonomo. Chiediamo
    # conferma SOLO per decisioni di prodotto o irreversibili.
    if force_classify and mode == "ask":
        is_product_or_irreversible = category in ("product", "irreversible") or not reversible
        if not is_product_or_irreversible:
            logger.info(
                "clarify_or_expand: auto + decisione %s reversibile -> procede autonomo (no domanda)",
                category,
            )
            return {}

    if mode == "ask":
        question = str(inp.get("question") or "").strip()
        if not question:
            return {}
        max_chars = int(cfg["max_question_chars"])
        if len(question) > max_chars:
            question = question[:max_chars].rsplit(" ", 1)[0] + "..."
        rationale = str(inp.get("rationale") or "")
        ms = meta_steps.make(
            kind="clarify",
            title="Serve un chiarimento",
            payload={
                "question": question,
                "rationale": rationale,
                "category": category,
                "reversible": reversible,
                "intent": state.get("user_intent"),
                "confidence": confidence,
            },
        )
        updates: dict[str, Any] = {"pending_clarify": True}
        if ms:
            updates["meta_steps"] = [ms]
            meta_steps.persist_async(run_id, ms)
        logger.info(
            "clarify_or_expand: ASK emesso run_id=%s confidence=%.2f question=%r",
            run_id, confidence, question[:60],
        )
        return updates

    if mode == "expand":
        expanded = str(inp.get("expanded_query") or "").strip()
        if not expanded or expanded == user_msg.strip():
            return {}
        logger.info(
            "clarify_or_expand: EXPAND run_id=%s confidence=%.2f orig_len=%d new_len=%d",
            run_id, confidence, len(user_msg), len(expanded),
        )
        return {"expanded_query": expanded}

    # mode=="skip" o sconosciuto
    return {}


def route_after_clarify(state: AgentState) -> str:
    """Conditional edge: se clarify ha emesso una domanda, fermiamo il turno.

    Restituisce "end" per terminare oppure "continue" per proseguire con il
    routing standard (planner o executor).
    """
    if state.get("pending_clarify"):
        return "end"
    return "continue"
