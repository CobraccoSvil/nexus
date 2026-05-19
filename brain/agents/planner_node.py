"""planner_node (PR-1 Plan/Act/Verify): produce la TODO list strutturata.

Si attiva PRIMA dell'executor quando:
  - `orchestrator_config.plan_phase_enabled` = True
  - behavior_mode ∈ `orchestrator_config.plan_behavior_modes`
  - intent ∈ `orchestrator_config.plan_intents`
  - token_budget >= `orchestrator_config.plan_min_token_budget`

Quando si attiva:
  1. Risolve il modello via nexus_purpose_model.planner (DB-driven, M60 cascade)
  2. Carica il prompt agent.planner.base da nexus_prompt_templates
  3. Esegue UNA chiamata LLM con tool_choice forzato su nexus_todo_write
  4. Esegue il tool_use via ToolRunner (persiste su nexus_agent_todos)
  5. Carica i todos appena creati e popola lo state per executor + reminder

Quando NON si attiva: pass-through (no_op), il loop legacy continua.

Errori: in caso di problemi (modello irraggiungibile, tool fallisce, parse
output non valido) il planner segna `plan_phase_active=False` con
`plan_phase_skip_reason` valorizzato cosi' il loop legacy prende il sopravvento
in fallback. NON blocca mai il run.
"""
from __future__ import annotations

import json
import logging
import uuid
from typing import Any

from langchain_core.messages import AIMessage, HumanMessage

from . import meta_steps, orchestrator_config, prompt_registry, todo_store
from .state import AgentState

logger = logging.getLogger(__name__)

# Servizi iniettati al configure_services() — riusiamo gli stessi singletoni
# del resto del package (vedi nodes.py:configure_services).
_providers = None
_tool_runner = None
_routing_client = None


def configure(providers: Any, tool_runner: Any, routing_client: Any) -> None:
    """Inject di provider registry, tool runner gRPC e routing client.

    Chiamato dal grpc_server alla startup, in parallelo a nodes.configure_services.
    """
    global _providers, _tool_runner, _routing_client
    _providers = providers
    _tool_runner = tool_runner
    _routing_client = routing_client


async def planner_node(state: AgentState) -> dict[str, Any]:
    """Nodo planner del grafo LangGraph (PR-1)."""
    # ── Guard: feature flag + eligibilita' (cache TTL 60s) ────────────────────
    cfg = orchestrator_config.get()
    behavior_mode = state.get("behavior_mode")
    intent = state.get("user_intent")
    token_budget = state.get("token_budget", 0)
    if not orchestrator_config.is_eligible(behavior_mode, intent, int(token_budget or 0)):
        logger.debug(
            "planner_node: skip (plan_enabled=%s mode=%s intent=%s budget=%s)",
            cfg["plan_phase_enabled"], behavior_mode, intent, token_budget,
        )
        return {"plan_phase_active": False}

    # Se siamo gia' passati dal planner per questo run e i todos esistono,
    # non rifare il piano (PR-2 gestira' le revisioni esplicite).
    run_id = state.get("thread_id")
    if run_id and todo_store.fetch_plan(run_id) is not None:
        todos = todo_store.list_todos(run_id)
        active = todo_store.active_todo(run_id)
        logger.info(
            "planner_node: plan gia' esistente per run_id=%s (%d todos), riuso",
            run_id, len(todos),
        )
        return {
            "plan_phase_active": True,
            "current_plan_id": run_id,
            "current_todos": todos,
            "active_todo_id": active.get("id") if active else None,
        }

    # ── Pre-requisiti: providers + tool_runner + routing_client ──────────────
    if _providers is None or _tool_runner is None or _routing_client is None:
        logger.warning(
            "planner_node: servizi non configurati (providers=%s tool_runner=%s router=%s) — skip",
            _providers is not None, _tool_runner is not None, _routing_client is not None,
        )
        return {"plan_phase_active": False, "plan_phase_skip_reason": "services_not_configured"}

    if not run_id:
        logger.warning("planner_node: thread_id assente — skip")
        return {"plan_phase_active": False, "plan_phase_skip_reason": "no_thread_id"}

    session_id = state.get("session_id")
    if not session_id:
        logger.warning("planner_node: session_id assente — skip")
        return {"plan_phase_active": False, "plan_phase_skip_reason": "no_session_id"}

    # ── PR-3 Codex pattern: clarifying questions pre-flight ─────────────────
    # In modalita' Confirm il planner emette `requires_clarification` con N
    # domande → loop si interrompe per HITL.
    # In Automatico/Continuo il planner applica default sensati e li annota
    # nel PRD/plan (trasparenza).
    pending_clar = state.get("pending_clarifications")
    if cfg.get("clarifying_questions_enabled", True) and pending_clar is None:
        try:
            clar_outcome = await _detect_clarifications(state, cfg)
        except Exception as exc:
            logger.debug("planner_node: clarifying detect skipped (%s)", exc)
            clar_outcome = None
        if clar_outcome and clar_outcome.get("questions"):
            # Solo Confirm si ferma per HITL; le altre modalita' applicano
            # i default e proseguono.
            is_confirm = behavior_mode in (None, "confirm", "study")
            if is_confirm:
                logger.info(
                    "planner_node: pending_clarifications emesse run_id=%s n=%d (HITL Confirm)",
                    run_id, len(clar_outcome["questions"]),
                )
                _persist_clarifications(run_id, state.get("project_id") or "", clar_outcome["questions"], applied=None)
                return {
                    "plan_phase_active": False,
                    "plan_phase_skip_reason": "awaiting_clarifications",
                    "pending_clarifications": clar_outcome["questions"],
                }
            else:
                # Applica default e prosegui.
                applied = {q["id"]: q.get("suggested_default", "") for q in clar_outcome["questions"]}
                _persist_clarifications(
                    run_id, state.get("project_id") or "",
                    clar_outcome["questions"], applied=applied,
                )
                state["applied_default_assumptions"] = list(clar_outcome["questions"])
                logger.info(
                    "planner_node: applied %d clarifying defaults run_id=%s (mode=%s)",
                    len(applied), run_id, behavior_mode,
                )

    # ── Risolvi provider/model via nexus_purpose_model.planner ───────────────
    # M69: se in un'iterazione precedente di questo run il cascade ha gia'
    # eletto un provider vincente per il planner, partiamo direttamente da
    # quello (sticky) invece di ri-tentare la chain completa.
    sticky_p = state.get("planner_sticky_provider")
    sticky_m = state.get("planner_sticky_model")
    if sticky_p and sticky_m:
        planner_provider = sticky_p
        planner_model = sticky_m
        logger.info(
            "planner_node: M69 sticky cascade attivo provider=%s model=%s",
            planner_provider, planner_model,
        )
    else:
        try:
            decision = _routing_client.purpose_model(purpose="planner")
            planner_provider = decision.provider
            planner_model = decision.model
        except Exception as exc:
            logger.error("planner_node: purpose_model(planner) fallito: %s — skip", exc)
            return {"plan_phase_active": False, "plan_phase_skip_reason": f"purpose_model_error:{exc}"}

    # ── Carica prompt dal registry DB (cache TTL 60s) ────────────────────────
    prompt_key = orchestrator_config.planner_prompt_key()
    system_text = prompt_registry.get_prompt(prompt_key) or ""
    if not system_text:
        logger.warning("planner_node: prompt '%s' non trovato in DB — skip", prompt_key)
        return {"plan_phase_active": False, "plan_phase_skip_reason": f"prompt_missing:{prompt_key}"}

    # ── Costruisci tool catalog minimale ─────────────────────────────────────
    # Per PR-1 esponiamo solo `nexus_todo_write` con tool_choice forzato.
    # In PR-2/3 estenderemo con read-only (list_files, read_file, recall_context).
    tools_json: list[dict] = [
        {
            "name": "nexus_todo_write",
            "description": "Crea la TODO list strutturata del piano. Chiamare UNA sola volta con action='create' e l'intera lista di todos atomici e verificabili.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["create"]},
                    "run_id": {"type": "string", "description": "UUID del run corrente (ti viene passato gia' valorizzato)"},
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {"type": "string"},
                                "status": {"type": "string", "enum": ["pending"]},
                                "priority": {"type": "string", "enum": ["high", "normal", "low"]},
                                "acceptance_criteria": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "type": {"type": "string"},
                                            "command": {"type": "string"},
                                            "expected": {"type": "string"},
                                            "url": {"type": "string"},
                                            "path": {"type": "string"},
                                        },
                                    },
                                },
                            },
                            "required": ["content"],
                        },
                    },
                    "planner_model": {"type": "string"},
                },
                "required": ["action", "run_id", "todos"],
            },
        }
    ]

    # ── Messaggi LLM ─────────────────────────────────────────────────────────
    messages = list(state.get("messages", []))
    # Iniettiamo il run_id nel system_text cosi' il modello sa quale valorizzare
    # nel tool_use (anche se il tool runner valida server-side).
    hinted_system = (
        system_text
        + f"\n\nRUN_ID corrente: {run_id} (usalo come parametro run_id nel tool nexus_todo_write)"
    )

    # ── LLM call attraverso registry sync (riusa cascade M60) ───────────────
    import asyncio as _asyncio
    try:
        anth_messages = _langchain_to_anthropic_messages_local(messages)
        prov_result = await _asyncio.to_thread(
            _providers.generate_agent_turn_sync,
            planner_provider, planner_model, anth_messages, tools_json,
            max_tokens=4096, system_text=hinted_system,
        )
    except Exception as exc:
        logger.error("planner_node: LLM call fallita: %s", exc)
        return {"plan_phase_active": False, "plan_phase_skip_reason": f"llm_error:{type(exc).__name__}"}

    meta = prov_result.metadata or {}
    pending_tool_uses = list(meta.get("tool_use_blocks") or [])
    used_provider = prov_result.provider or planner_provider
    used_model = prov_result.model or planner_model

    # Estrai il tool_use per nexus_todo_write
    todo_block = next((b for b in pending_tool_uses if b.get("name") == "nexus_todo_write"), None)
    if todo_block is None:
        logger.warning(
            "planner_node: il modello non ha emesso nexus_todo_write (pending=%d) — skip",
            len(pending_tool_uses),
        )
        return {
            "plan_phase_active": False,
            "plan_phase_skip_reason": "no_tool_use_emitted",
        }

    # Esegui il tool via ToolRunner (audit + isolation server-side)
    tool_input = dict(todo_block.get("input") or {})
    tool_input["run_id"] = run_id  # forza valorizzazione corretta
    tool_input.setdefault("planner_model", f"{used_provider}/{used_model}")
    tool_use_id = todo_block.get("id") or str(uuid.uuid4())
    try:
        result = await _tool_runner.execute_tool(
            tool_name="nexus_todo_write",
            tool_input=tool_input,
            session_id=str(session_id),
            tool_use_id=tool_use_id,
        )
    except Exception as exc:
        logger.error("planner_node: execute_tool nexus_todo_write fallita: %s", exc)
        return {"plan_phase_active": False, "plan_phase_skip_reason": f"tool_error:{type(exc).__name__}"}

    # Parse risultato tool
    try:
        result_obj = json.loads(result.result_json or "{}")
    except json.JSONDecodeError:
        result_obj = {"ok": False, "raw": result.result_json}

    if not result_obj.get("ok"):
        logger.warning("planner_node: tool ritorna errore: %s", result.result_json[:200])
        return {
            "plan_phase_active": False,
            "plan_phase_skip_reason": "tool_returned_error",
        }

    # Ricarica todos appena persistiti per popolare lo state
    todos = todo_store.list_todos(run_id)
    active = todo_store.active_todo(run_id)

    logger.info(
        "planner_node: plan creato run_id=%s todos=%d provider=%s model=%s active_todo=%s",
        run_id, len(todos), used_provider, used_model,
        active.get("seq") if active else None,
    )

    # ── Meta-step `plan` per pubblicazione in chat ──────────────────────────
    plan_meta = meta_steps.make(
        kind="plan",
        title=f"Piano creato — {len(todos)} step",
        payload={
            "plan_id": run_id,
            "todos": [
                {
                    "id": t.get("id"),
                    "seq": t.get("seq"),
                    "content": t.get("content"),
                    "status": t.get("status"),
                    "priority": t.get("priority"),
                }
                for t in todos
            ],
            "provider": used_provider,
            "model": used_model,
            "active_todo_id": (active.get("id") if active else None),
        },
    )
    plan_meta_list = [plan_meta] if plan_meta else []
    if plan_meta:
        meta_steps.persist_async(run_id, plan_meta)

    # Costruzione assistant message + tool_result message per continuity
    # della conversazione (cosi' il prossimo turno dell'executor vede il plan).
    assistant_content = meta.get("assistant_content")
    assistant_msg = AIMessage(
        content=prov_result.content or "",
        additional_kwargs={"anthropic_content": assistant_content} if assistant_content else {},
    )
    tool_result_msg = HumanMessage(
        content="",
        additional_kwargs={
            "anthropic_content": [
                {
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": result.result_json or "",
                    "is_error": False,
                }
            ]
        },
    )

    return {
        "plan_phase_active": True,
        "current_plan_id": run_id,
        "current_todos": todos,
        "active_todo_id": (active.get("id") if active else None),
        "messages": [assistant_msg, tool_result_msg],
        "provider_used": used_provider,
        "model_used": used_model,
        # M69: persisti il provider/model effettivamente vincente del cascade
        # cosi' eventuali replan futuri di questo run lo riusano direttamente.
        "planner_sticky_provider": used_provider,
        "planner_sticky_model": used_model,
        "meta_steps": plan_meta_list,
    }


async def _detect_clarifications(state: AgentState, cfg: dict) -> dict[str, Any] | None:
    """PR-3 Codex pattern: chiede al LLM se il task utente e' ambiguo.

    Ritorna `{"questions": [{id,question,suggested_default}, ...]}` se ambiguo,
    altrimenti None. Best-effort: in caso di errore non blocca il run.
    """
    max_q = int(cfg.get("clarifying_questions_max", 3))
    # Recupera l'ultimo messaggio user (il task vero e proprio).
    user_msg = ""
    for m in reversed(state.get("messages", []) or []):
        if getattr(m, "type", None) in ("human", "user"):
            content = getattr(m, "content", "") or ""
            user_msg = content if isinstance(content, str) else str(content)
            break
    if not user_msg.strip():
        return None
    template = prompt_registry.get_prompt("agent.clarifying.detect") or ""
    if not template:
        return None
    # Render template con max_q.
    from . import prompt_renderer
    system_text = prompt_renderer.render(template, {"max_questions": max_q})
    # Usa lo stesso routing del planner (deepseek o sticky).
    try:
        decision = _routing_client.purpose_model(purpose="planner")
        provider, model = decision.provider, decision.model
    except Exception:
        return None
    # Tool semplice per la struttura della risposta.
    tools_json = [{
        "name": "request_clarification",
        "description": "Emetti questa lista di domande se e SOLO se il task utente e' ambiguo.",
        "input_schema": {
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string"},
                            "question": {"type": "string"},
                            "suggested_default": {"type": "string"},
                        },
                        "required": ["id", "question"],
                    },
                }
            },
            "required": ["questions"],
        },
    }]
    import asyncio as _aio
    try:
        anth_messages = _langchain_to_anthropic_messages_local([
            type("M", (), {"type": "human", "content": user_msg, "additional_kwargs": {}})(),
        ])
        result = await _aio.to_thread(
            _providers.generate_agent_turn_sync,
            provider, model, anth_messages, tools_json,
            max_tokens=512, system_text=system_text,
        )
    except Exception as exc:
        logger.debug("_detect_clarifications: LLM call fallita: %s", exc)
        return None
    meta = result.metadata or {}
    blocks = meta.get("tool_use_blocks") or []
    for b in blocks:
        if b.get("name") == "request_clarification":
            inp = b.get("input") or {}
            qs = inp.get("questions") or []
            qs = [q for q in qs if isinstance(q, dict) and q.get("id") and q.get("question")][:max_q]
            if qs:
                return {"questions": qs}
    return None


def _persist_clarifications(
    run_id: str,
    project_id: str,
    questions: list[dict],
    applied: dict | None,
) -> None:
    """Persisti la riga in nexus_agent_clarifications (best-effort)."""
    import os
    url = os.environ.get("DATABASE_URL", "")
    if not url:
        return
    try:
        import psycopg2  # type: ignore[import-untyped]
        import json as _json
        with psycopg2.connect(url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    """INSERT INTO nexus_agent_clarifications
                       (run_id, project_id, questions, applied_defaults, answered_at)
                       VALUES (%s, %s, %s::jsonb, %s::jsonb, %s)""",
                    (
                        run_id, project_id,
                        _json.dumps(questions),
                        _json.dumps(applied) if applied is not None else None,
                        "NOW()" if applied is not None else None,
                    ),
                )
            conn.commit()
    except Exception as exc:
        logger.debug("_persist_clarifications fallita: %s", exc)


def _langchain_to_anthropic_messages_local(messages: list) -> list[dict]:
    """Conversione minimale langchain → Anthropic messages.

    Duplicato locale leggero per evitare dipendenza circolare con
    nodes.py:_langchain_to_anthropic_messages (che pero' ha logica equivalente).
    """
    out = []
    for m in messages:
        role = getattr(m, "type", "human")
        anth_role = {"human": "user", "ai": "assistant", "system": "system"}.get(role, "user")
        content = m.content if hasattr(m, "content") else str(m)
        anth_content = (m.additional_kwargs or {}).get("anthropic_content") if hasattr(m, "additional_kwargs") else None
        if anth_content:
            out.append({"role": anth_role, "content": anth_content})
        else:
            out.append({"role": anth_role, "content": content})
    return out
