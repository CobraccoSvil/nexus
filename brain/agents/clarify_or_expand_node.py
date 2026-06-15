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
import uuid
from typing import Any

from . import meta_steps, prompt_registry
from .state import AgentState

logger = logging.getLogger(__name__)

# Iniettati da configure(); riusiamo gli stessi singletoni di planner_node.
_providers: Any = None
_routing_client: Any = None
# Tool runner gRPC (stesso singleton di verifier/understanding) per esplorare
# il workspace e dedurre il dominio invece di chiedere all'utente.
_tool_runner: Any = None

# Endpoint interno no-auth gia' usato altrove: riuso del servizio di ricerca
# vettoriale esistente (condizione di integrazione, niente client nuovo).
_MCP_CORE_INTERNAL_URL = os.environ.get("MCP_CORE_INTERNAL_URL", "http://localhost:4000")

# Marcatori di dominio/codice/design: la loro presenza nel listing top-level del
# progetto indica un progetto ESISTENTE da cui dedurre dominio ed entita',
# rendendo superflua la domanda all'utente.
_DOMAIN_MARKERS: tuple[tuple[str, str], ...] = (
    ("figma_export", "design importato (figma_export/)"),
    ("src", "codice sorgente (src/)"),
    ("app", "codice applicativo (app/)"),
    ("lib", "codice di libreria (lib/)"),
    ("package.json", "progetto Node/JS (package.json)"),
    ("requirements.txt", "progetto Python (requirements.txt)"),
    ("pyproject.toml", "progetto Python (pyproject.toml)"),
    ("go.mod", "progetto Go (go.mod)"),
    ("cargo.toml", "progetto Rust (Cargo.toml)"),
    ("pom.xml", "progetto Java/Maven (pom.xml)"),
    ("readme", "documentazione di progetto (README)"),
    (".csproj", "progetto .NET (*.csproj)"),
    ("composer.json", "progetto PHP (composer.json)"),
    ("gemfile", "progetto Ruby (Gemfile)"),
)


def configure(providers: Any, routing_client: Any, tool_runner: Any = None) -> None:
    """Iniettato dal grpc_server in startup, in parallelo a planner_node.

    `tool_runner` (opzionale) abilita l'esplorazione leggera del workspace per
    dedurre il dominio dei progetti esistenti (vedi `_build_project_context`).
    """
    global _providers, _routing_client, _tool_runner
    _providers = providers
    _routing_client = routing_client
    _tool_runner = tool_runner


def _load_config() -> dict[str, Any]:
    """Carica config clarify dalla tabella settings (cache 60s in
    orchestrator_config). Restituisce default se DB irraggiungibile."""
    defaults = {
        "enabled": True,
        "confidence_threshold": 0.6,
        "require_llm_classifier": False,
        "prompt_key": "agent.clarify.base",
        "max_question_chars": 280,
        # Tetto ai tentativi di clarify per run (mig 0386): oltre la soglia
        # l'agente procede col candidato top invece di ri-chiedere (fail-open
        # verso l'azione), eliminando il loop di disambiguazione ripetuta.
        "max_attempts": 1,
        # Cluster 4: lookup decisione gia' presa + conferma decisioni di prodotto.
        "decision_lookup_enabled": False,
        "decision_min_score": 0.7,
        "decision_topk": 5,
        "confirm_irreversible_in_auto": False,
        # Comp.1: Intake Gate multi-asse (assorbe il decision-lookup quando ON).
        "intake_gate_enabled": False,
        "intake_match_min_score": 0.7,
        "intake_topk": 5,
        # M14.4: conferma su duplicate gia' implementati-e-verificati. Chiave DB
        # clarify.confirm_if_implemented (rinominata da kb.intake.* in mig 0408:
        # il loader legge SOLO clarify.% e la chiave non veniva mai caricata —
        # bug di wiring dalla nascita, audit settings 2026-06-11).
        "confirm_if_implemented": True,
        # Sotto (<=) questo agentic_score un intent chat e' small-talk puro
        # (saluti/ringraziamenti) e bypassa l'intake gate. Sopra, anche una
        # "chat" e' una richiesta sostanziale e consulta la KB del progetto.
        "smalltalk_agentic_score_max": 0.3,
    }
    url = os.environ.get("DATABASE_URL")
    if not url:
        return defaults
    try:
        from brain.utils.db_pool import connect as _db_connect
        with _db_connect() as conn:
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
                    elif short == "max_attempts":
                        try:
                            defaults["max_attempts"] = int(value)
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
                    elif short == "intake_gate_enabled":
                        defaults["intake_gate_enabled"] = str(value).lower() not in ("false", "0", "off")
                    elif short == "intake_match_min_score":
                        try:
                            defaults["intake_match_min_score"] = float(value)
                        except (TypeError, ValueError):
                            pass
                    elif short == "intake_topk":
                        try:
                            defaults["intake_topk"] = int(value)
                        except (TypeError, ValueError):
                            pass
                    elif short == "smalltalk_agentic_score_max":
                        try:
                            defaults["smalltalk_agentic_score_max"] = float(value)
                        except (TypeError, ValueError):
                            pass
                    elif short == "confirm_if_implemented":
                        defaults["confirm_if_implemented"] = str(value).lower() not in ("false", "0", "off")
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


def _intake_gate(state: AgentState, user_msg: str, cfg: dict) -> dict | None:
    """Comp.1: gate di intake multi-asse. UNA ricerca KB (servizio esistente) +
    UNA classificazione LLM della RELAZIONE della richiesta con la knowledge:
    nuova | duplicate | refinement | correction. Assorbe il decision-lookup del
    Cluster 4. Sincrona (chiamata via asyncio.to_thread dal nodo). Best-effort:
    in caso di errore o nessun match ritorna {"relation": "nuova"} per non
    bloccare il run.
    """
    if not cfg.get("intake_gate_enabled"):
        return None
    project_id = str(state.get("project_id") or "").strip()
    if not project_id or len(user_msg) < 10:
        return None
    # 1. Ricerca KB tramite l'endpoint interno esistente (niente client nuovo).
    try:
        import requests  # noqa: PLC0415
        resp = requests.post(
            f"{_MCP_CORE_INTERNAL_URL}/api/internal/knowledge/search",
            json={
                "project_id": project_id,
                "query": user_msg,
                "top_k": int(cfg.get("intake_topk", 5)),
                "min_score": float(cfg.get("intake_match_min_score", 0.7)),
            },
            timeout=5,
        )
        if resp.status_code != 200:
            return {"relation": "nuova", "related": None, "candidates": []}
        results = resp.json().get("results", []) or []
    except Exception as exc:
        logger.debug("intake_gate: ricerca KB fallita (%s)", exc)
        return {"relation": "nuova", "related": None, "candidates": []}
    if not results:
        return {"relation": "nuova", "related": None, "candidates": []}

    # 2. Classificazione LLM multi-asse (UNA call) sul set di candidati.
    if _providers is None or _routing_client is None:
        return {"relation": "nuova", "related": None, "candidates": results}
    try:
        decision = _routing_client.purpose_model(purpose="intake_gate")
        provider, model = decision.provider, decision.model
        # Sentinella gate (ADR 0020): __router_unavailable__ o
        # __no_capable_provider__ (purpose su provider in cooldown). Niente
        # provider valido -> skip enrichment invece di chiamare un provider morto.
        if not provider or provider.startswith("__"):
            logger.debug("intake_gate: nessun provider disponibile (%s), skip", provider)
            return {"relation": "nuova", "related": None, "candidates": results}
    except Exception as exc:
        logger.debug("intake_gate: purpose_model fallito (%s)", exc)
        return {"relation": "nuova", "related": None, "candidates": results}

    cand_lines = []
    for i, r in enumerate(results[:5]):
        snippet = (r.get("snippet") or "")[:200]
        cand_lines.append(f"[{i}] ({r.get('intent', '?')}) {r.get('title', '')}: {snippet}")
    cand_text = "\n".join(cand_lines)
    from . import prompt_registry
    _intake_default = (
        "Sei un classificatore di intake per un progetto software. Data una NUOVA "
        "richiesta dell'utente e le note ESISTENTI nella knowledge base del progetto, "
        "determina la RELAZIONE della richiesta con quanto gia' presente:\n"
        "- nuova: argomento non coperto dalle note esistenti.\n"
        "- duplicate: gia' fatto/elaborato (la richiesta ripete qualcosa di presente).\n"
        "- refinement: amplia o estende una nota esistente (stesso tema, piu' dettaglio).\n"
        "- correction: contraddice o cambia una decisione/feature esistente.\n"
        "Imposta related_index all'indice [n] della nota piu' pertinente (-1 se nuova). "
        "Imposta off_topic=true se la richiesta NON riguarda lo scopo del progetto. "
        "Rispondi SOLO chiamando il tool intake_classify."
    )
    # System prompt dal DB (system.intake_classifier, mig 0444) con fallback alla
    # costante (graceful degradation se il registry e' vuoto / DB down).
    system_text = prompt_registry.get_prompt("system.intake_classifier") or _intake_default
    tools_json = [{
        "name": "intake_classify",
        "description": "Classifica la relazione della richiesta con la knowledge base esistente.",
        "input_schema": {
            "type": "object",
            "properties": {
                "relation": {"type": "string", "enum": ["nuova", "duplicate", "refinement", "correction"]},
                "related_index": {"type": "integer", "description": "Indice [n] della nota piu' pertinente, -1 se nuova"},
                "off_topic": {"type": "boolean", "description": "True se la richiesta non e' pertinente allo scopo del progetto."},
                "rationale": {"type": "string"},
            },
            "required": ["relation"],
        },
    }]
    user_block = f"NUOVA RICHIESTA:\n{user_msg}\n\nNOTE ESISTENTI:\n{cand_text}"
    try:
        # Clamp difensivo del single prompt (punto unico, regola L):
        # cand_text concatena tutte le note esistenti -> puo' diventare grande.
        from brain.agents.context_brake import clamp_single_prompt

        user_block = clamp_single_prompt(user_block, model)
        result = _providers.generate_agent_turn_sync(
            provider, model, [{"role": "user", "content": user_block}], tools_json,
            max_tokens=400, system_text=system_text,
        )
    except Exception as exc:
        logger.warning("intake_gate: LLM call fallita (%s)", exc)
        return {"relation": "nuova", "related": None, "candidates": results}

    meta = result.metadata or {}
    blocks = meta.get("tool_use_blocks") or []
    block = next((b for b in blocks if b.get("name") == "intake_classify"), None)
    if block is None:
        return {"relation": "nuova", "related": None, "candidates": results}
    inp = block.get("input") or {}
    relation = str(inp.get("relation") or "nuova").lower()
    if relation not in ("nuova", "duplicate", "refinement", "correction"):
        relation = "nuova"
    related = None
    idx = inp.get("related_index")
    try:
        if idx is not None and 0 <= int(idx) < len(results):
            related = results[int(idx)]
    except (TypeError, ValueError):
        related = None
    if related is None and relation != "nuova":
        related = results[0]
    return {
        "relation": relation,
        "related": related,
        "candidates": results,
        "off_topic": bool(inp.get("off_topic", False)),
        "rationale": str(inp.get("rationale") or ""),
    }


def _note_implementation_status(note_id: str | None) -> dict[str, Any]:
    """M14.4 — Risolve lo stato di implementazione di una nota: segue
    source_run_id -> agent_runs.status e ritorna {implemented, run_status,
    completed_at}. Best-effort: in caso di errore/DB down ritorna {}.
    """
    if not note_id:
        return {}
    url = os.environ.get("DATABASE_URL")
    if not url:
        return {}
    try:
        from brain.utils.db_pool import connect as _db_connect
        with _db_connect() as conn:
            with conn.cursor() as cur:
                # Il legame nota->run e' duplice: source_run_id (se valorizzato,
                # es. agent_summary) OPPURE, per le note 'chat' della richiesta
                # utente, source_message_id == agent_runs.run_message_id (le note
                # chat nascono senza source_run_id). Si privilegia un run
                # 'completed' e il piu' recente.
                cur.execute(
                    "SELECT r.status, r.completed_at, r.id "
                    "FROM project_knowledge_notes n "
                    "JOIN agent_runs r ON ("
                    "  r.id = n.source_run_id "
                    "  OR (r.run_message_id = n.source_message_id "
                    "      AND r.project_id = n.project_id)"
                    ") "
                    "WHERE n.id = %s "
                    "ORDER BY (r.status = 'completed') DESC, r.completed_at DESC NULLS LAST "
                    "LIMIT 1",
                    (note_id,),
                )
                row = cur.fetchone()
                if not row:
                    return {}
                status = str(row[0] or "")
                return {
                    "implemented": status == "completed",
                    "run_status": status,
                    "completed_at": str(row[1]) if row[1] else None,
                    "run_id": str(row[2]) if row[2] else None,
                }
    except Exception as exc:
        logger.debug("intake_gate: _note_implementation_status fallita (%s)", exc)
        return {}


def _apply_intake_verdict(state: AgentState, intake: dict, cfg: dict) -> dict[str, Any] | None:
    """Comp.1: traduce il verdetto del gate in azioni di flusso (riusa il
    meccanismo clarify/meta_step, niente nuovo HITL). Ritorna gli updates da
    propagare e, se necessario, ferma il turno (pending_clarify). Ritorna None
    quando la richiesta e' 'nuova' e il flusso deve proseguire normalmente.
    """
    relation = str(intake.get("relation") or "nuova")
    related = intake.get("related") or {}
    rationale = intake.get("rationale") or ""
    run_id = state.get("thread_id")
    automation = (state.get("automation_mode") or "").strip().lower()
    is_auto = automation in ("automatic", "automatico", "auto", "continuous", "continuo")

    # Pertinenza: richiesta fuori tema rispetto allo scopo del progetto. Segnala
    # (meta_step) e prosegue: la nota resta in KB, l'agente puo' marcarla
    # off_topic con knowledge_set_relevance per escluderla dal grafo/DAG.
    if intake.get("off_topic"):
        ms = meta_steps.make(
            kind="clarify",
            title="Richiesta fuori tema rispetto al progetto",
            payload={"mode": "intake_off_topic", "rationale": rationale},
        )
        updates: dict[str, Any] = {}
        if ms:
            updates["meta_steps"] = [ms]
            meta_steps.persist_async(run_id, ms)
        logger.info("intake_gate: richiesta off-topic segnalata")
        return updates

    if relation == "nuova":
        return None

    if relation == "duplicate":
        # M14.4 request-aware: risolve se la nota duplicata e' stata davvero
        # implementata (run completed) per arricchire il verdetto e la storia.
        impl = _note_implementation_status(related.get("note_id"))
        ms = meta_steps.make(
            kind="clarify",
            title="Richiesta gia' elaborata",
            payload={
                "mode": "intake_duplicate",
                "relation": relation,
                "source_note_id": related.get("note_id"),
                "source_title": related.get("title"),
                "score": related.get("score"),
                "rationale": rationale,
                # M14.4: storia/stato implementazione (vuoto se non risolvibile).
                "implemented": impl.get("implemented"),
                "run_status": impl.get("run_status"),
                "completed_at": impl.get("completed_at"),
            },
        )
        updates: dict[str, Any] = {}
        if ms:
            updates["meta_steps"] = [ms]
            meta_steps.persist_async(run_id, ms)
        # M14.4: se la richiesta e' GIA' implementata-e-verificata, chiede sempre
        # conferma prima di rifarla, ANCHE in automatico (decisione utente:
        # evitare di rifare lavoro gia' fatto). Gated clarify.confirm_if_implemented.
        confirm_if_impl = bool(cfg.get("confirm_if_implemented", True))
        force_confirm = confirm_if_impl and impl.get("implemented")
        if not is_auto or force_confirm:
            updates["pending_clarify"] = True
        logger.info(
            "intake_gate: duplicate di '%s' (auto=%s, implemented=%s, force_confirm=%s)",
            related.get("title"), is_auto, impl.get("implemented"), force_confirm,
        )
        return updates

    if relation == "correction":
        # Auto senza conferma irreversibili: segnala e prosegue (non blocca).
        if is_auto and not cfg.get("confirm_irreversible_in_auto"):
            ms = meta_steps.make(
                kind="clarify",
                title="Possibile contraddizione (procedo)",
                payload={
                    "mode": "intake_correction_auto",
                    "source_note_id": related.get("note_id"),
                    "source_title": related.get("title"),
                    "rationale": rationale,
                },
            )
            updates = {}
            if ms:
                updates["meta_steps"] = [ms]
                meta_steps.persist_async(run_id, ms)
            return updates
        # Altrimenti ferma per conferma esplicita.
        question = (
            f"Questa richiesta sembra contraddire una scelta esistente: "
            f"'{related.get('title', '')}'. Confermi il cambiamento?"
        )
        ms = meta_steps.make(
            kind="clarify",
            title="La richiesta contraddice una scelta esistente",
            payload={
                "mode": "intake_correction",
                "source_note_id": related.get("note_id"),
                "source_title": related.get("title"),
                "rationale": rationale,
                "question": question,
            },
        )
        updates = {"pending_clarify": True}
        if ms:
            updates["meta_steps"] = [ms]
            meta_steps.persist_async(run_id, ms)
        logger.info("intake_gate: correction vs '%s' -> conferma", related.get("title"))
        return updates

    # refinement: emette meta_step e prosegue (non blocca).
    ms = meta_steps.make(
        kind="clarify",
        title="Ampliamento di una nota esistente",
        payload={
            "mode": "intake_refinement",
            "source_note_id": related.get("note_id"),
            "source_title": related.get("title"),
            "rationale": rationale,
        },
    )
    updates = {}
    if ms:
        updates["meta_steps"] = [ms]
        meta_steps.persist_async(run_id, ms)
    logger.info("intake_gate: refinement di '%s' -> prosegue", related.get("title"))
    return updates


async def _build_project_context(state: AgentState) -> str:
    """Esplora in modo leggero il progetto (UNA `list_files` sulla root) e
    rileva marcatori di dominio/codice/design. Ritorna un breve blocco testuale
    da iniettare nel prompt del clarify, oppure "" se non c'e' nulla da segnalare
    o se il tool_runner non e' configurato.

    Best-effort: qualunque errore -> "" (comportamento storico, nessun blocco).
    NON legge file pesanti: solo il listing top-level (1 chiamata).
    """
    if _tool_runner is None:
        return ""
    session_id = str(state.get("session_id") or "")
    if not session_id:
        return ""
    try:
        import asyncio as _aio_ctx
        res = await _aio_ctx.wait_for(
            _tool_runner.execute_tool(
                tool_name="list_files",
                tool_input={"directory": "."},
                session_id=session_id,
                tool_use_id=str(uuid.uuid4()),
            ),
            timeout=5,
        )
        raw = getattr(res, "result_json", None) or ""
    except Exception as exc:
        logger.debug("clarify_or_expand: list_files root fallita (%s)", exc)
        return ""

    if not raw or raw.startswith("❌") or "[Errore" in raw[:30] or "[Error" in raw[:30]:
        return ""

    raw_lower = raw.lower()
    found: list[str] = []
    seen: set[str] = set()
    for marker, label in _DOMAIN_MARKERS:
        if marker in raw_lower and label not in seen:
            found.append(label)
            seen.add(label)
    if not found:
        return ""
    logger.info("clarify_or_expand: project_context rilevato (%d marcatori)", len(found))
    return (
        "CONTESTO PROGETTO: il workspace contiene gia' "
        + ", ".join(found)
        + ". Si tratta di un progetto ESISTENTE: dominio, entita' e stack sono "
        "DEDUCIBILI esplorando questi file. NON chiedere all'utente la natura "
        "dell'applicazione ne' le entita'."
    )


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

    # Disambiguazione gia' risolta (l'utente ha risposto A/B): mcp-core imposta
    # intent_hint (+ confidence=1.0) e il router_node lo usa al posto di
    # ri-classificare. Senza questa guardia, clarify ri-valuta la confidence
    # (ancora bassa dal turno ambiguo) e RIPROPONE la stessa domanda A/B. Punto
    # unico (regola L) che impedisce di richiedere il chiarimento dopo la risposta.
    if state.get("intent_hint"):
        logger.info(
            "clarify_or_expand: intent_hint=%s gia' risolto (disambiguazione mcp-core) -> skip",
            state.get("intent_hint"),
        )
        return {}

    # Intent CONVERSAZIONALE: una richiesta di chiacchierata/discussione NON e'
    # una richiesta operativa ambigua da chiarire. Per chat/general_chat si
    # risponde direttamente (coerente col router_node che azzera i tool): niente
    # clarify, niente intake gate, niente decision lookup. Senza questa guardia
    # il nodo chiamava un LLM extra con tool clarify_or_expand che, su risposta
    # malformata, emetteva un "Serve un chiarimento" fuorviante e innescava la
    # cascade di fallback provider (giro a vuoto). Punto unico della decisione
    # "questa richiesta non va chiarita perche' e' conversazionale".
    intent = str(state.get("user_intent") or "").strip().lower()
    if intent in ("chat", "general_chat"):
        # Bypass SOLO per lo small-talk puro (saluti/ringraziamenti): basso
        # agentic_score dal classifier LLM. Una domanda sostanziale finita in
        # "chat" NON viene bypassata: deve passare per l'intake gate esistente,
        # che consulta la KB del progetto (richiesta nuova/duplicata + coerenza
        # off_topic). Niente keyword: la distinzione e' semantica (agentic_score).
        # Gli intent agentic_default (LLM in timeout) non sono "chat", quindi non
        # arrivano qui: proseguono gia' all'intake gate.
        agentic_score = float(state.get("agentic_score") or 0.0)
        smalltalk_max = float(cfg.get("smalltalk_agentic_score_max", 0.3))
        if agentic_score <= smalltalk_max:
            logger.info(
                "clarify_or_expand: intent=%s small-talk (agentic_score=%.2f<=%.2f) -> skip",
                intent, agentic_score, smalltalk_max,
            )
            return {}
        logger.info(
            "clarify_or_expand: intent=%s ma richiesta sostanziale (agentic_score=%.2f) -> intake gate/KB",
            intent, agentic_score,
        )

    user_msg_preview = _last_user_message(state).strip()

    # ── Comp.1: Intake Gate multi-asse (gated). Assorbe il decision-lookup. ───
    if cfg.get("intake_gate_enabled"):
        import asyncio as _aio_gate
        intake = await _aio_gate.to_thread(_intake_gate, state, user_msg_preview, cfg)
        if intake is not None:
            verdict = _apply_intake_verdict(state, intake, cfg)
            if verdict is not None:
                return verdict
        # 'nuova' -> prosegue il flusso clarify standard sotto.

    # ── Cluster 4: la decisione e' GIA' stata presa? (RAG, gated) ────────────
    # Cerca decisioni passate; se ne trova una con score alto, la applica
    # (meta_step resolved_from_memory) e prosegue SENZA chiedere. Saltato quando
    # il gate intake e' attivo (lo assorbe, evita la doppia ricerca).
    existing_decision = (
        None if cfg.get("intake_gate_enabled")
        else _lookup_existing_decision(state, user_msg_preview, cfg)
    )
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
    # Tetto ai tentativi di clarify per run (mig 0386, fail-open verso l'azione).
    # Oltre la soglia NON ri-chiediamo la stessa cosa: l'agente procede col
    # candidato a confidence piu' alta. Elimina il loop di disambiguazione
    # ripetuta (la domanda "A/B" emessa due volte identica).
    _clarify_attempts = int(state.get("clarify_attempts") or 0)
    _max_clarify = int(cfg.get("max_attempts") or 1)
    if _clarify_attempts >= _max_clarify:
        logger.info(
            "clarify_or_expand: tetto tentativi raggiunto (%d/%d) -> fail-open, "
            "procedo senza ri-chiedere",
            _clarify_attempts, _max_clarify,
        )
        return {}
    confidence = float(state.get("intent_confidence") or 1.0)
    logger.info(
        "clarify_or_expand: entrata run_id=%s confidence=%.2f threshold=%.2f",
        state.get("thread_id"), confidence, float(cfg["confidence_threshold"]),
    )
    threshold = float(cfg["confidence_threshold"])

    # L'interpretazione semantica del testo passa SOLO per il classifier LLM
    # (brain/router/agentic_classifier.py): la sua confidence in intent_confidence
    # e' l'unico segnale di ambiguita'. Niente matching di keyword/stringhe come
    # trigger secondario (coerente con la rimozione di _classify_by_keywords nel
    # SemanticRouter, brain/router/service.py).
    # force_classify (Cluster 4): in automatico con conferma decisioni di
    # prodotto attiva, classifichiamo SEMPRE per intercettare le decisioni
    # irreversibili, anche con confidence alta.
    if confidence >= threshold and not force_classify:
        logger.info(
            "clarify_or_expand: skip (confidence %.2f >= %.2f)",
            confidence, threshold,
        )
        return {}
    logger.info(
        "clarify_or_expand: trigger (confidence=%.2f msg=%r)",
        confidence, user_msg_preview[:60],
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
        # Sentinella gate (ADR 0020): nessun provider disponibile (irraggiungibile
        # o purpose su provider in cooldown) -> skip invece di chiamare un morto.
        if not provider or provider.startswith("__"):
            logger.info("clarify_or_expand: nessun provider disponibile (%s), skip", provider)
            return {}
        logger.info("clarify_or_expand: purpose_model -> %s/%s", provider, model)
    except Exception as exc:
        logger.warning("clarify_or_expand: purpose_model fallito (%s), skip", exc)
        return {}

    system_text = prompt_registry.get_prompt(cfg["prompt_key"]) or ""
    if not system_text:
        logger.debug("clarify_or_expand: prompt '%s' non trovato, skip", cfg["prompt_key"])
        return {}

    # CONTESTO PROGETTO (esplorazione leggera): solo ora che il clarify e' gia'
    # triggerato (NON su ogni messaggio). Una list_files top-level per dedurre
    # se il dominio e' gia' presente nei file -> prompt sceglie mode=skip.
    project_context = await _build_project_context(state)
    if project_context:
        system_text = system_text + "\n\n" + project_context

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

    # Clamp difensivo del single prompt (punto unico, regola L): l'user_msg
    # qui e' il primo messaggio dell'utente, puo' includere allegati incollati.
    from brain.agents.context_brake import clamp_single_prompt

    clamped_user_msg = clamp_single_prompt(user_msg, model)
    anth_messages = [{"role": "user", "content": clamped_user_msg}]
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
        # Incrementa il contatore di clarify del run (mig 0386): al prossimo
        # ingresso nel nodo il tetto max_attempts fara' fail-open verso l'azione
        # invece di ri-emettere la stessa domanda.
        updates: dict[str, Any] = {
            "pending_clarify": True,
            "clarify_attempts": _clarify_attempts + 1,
        }
        if ms:
            updates["meta_steps"] = [ms]
            meta_steps.persist_async(run_id, ms)
        logger.info(
            "clarify_or_expand: ASK emesso run_id=%s confidence=%.2f question=%r "
            "(tentativo %d/%d)",
            run_id, confidence, question[:60], _clarify_attempts + 1, _max_clarify,
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
