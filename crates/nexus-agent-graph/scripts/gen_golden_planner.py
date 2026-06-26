#!/usr/bin/env python3
"""Genera il golden di parita' 1:1 per il PlannerNode Rust.

Importa/replica le funzioni DETERMINISTICHE + i rami ON-di-default del planner
(`brain/agents/planner_node.py` + `brain/agents/orchestrator_config.py`):

  - is_eligible          -> funzione REALE `orchestrator_config.is_eligible`,
                            resa DB-free monkeypatchando `orchestrator_config.get`.
  - plan_reuse           -> replica byte-fedele della decisione di riuso piano
                            (planner_node.py:84-100).
  - clarifying_branch    -> replica byte-fedele del branching clarifying
                            (planner_node.py:144-170).
  - tool_catalog         -> replica della costante nexus_todo_write
                            (planner_node.py:221-290).
  - build_hinted_system  -> replica della concatenazione dei SOLI rami ON
                            (planner_node.py:296-366) usando il VERO
                            build_turn_focus_directive (helpers.py).
  - build_tool_input     -> replica (planner_node.py:495-503).
  - parse_tool_result    -> replica del gate result_obj.get("ok")
                            (planner_node.py:517-527).

Output: /tmp/golden_planner.json — lista di {case_id, function, input, output}
consumata dal test Rust `golden::golden_planner_parita`.

Nessun accesso al DB (is_eligible usa il cfg monkeypatchato). I rami OFF
(RAG/backlog/dag_kb/rationale) NON sono coperti: con i default OFF il Python NON
li attraversa, quindi build_hinted_system produce la stessa concatenazione del
Rust (parita').

Uso:
  python3 crates/nexus-agent-graph/scripts/gen_golden_planner.py
  cargo test -p nexus-agent-graph --lib golden_planner_parita -- --ignored
"""
from __future__ import annotations

import json
import os
import sys

# Rende importabile il package `brain` dalla root del repo.
_REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
if _REPO_ROOT not in sys.path:
    sys.path.insert(0, _REPO_ROOT)

from langchain_core.messages import AIMessage, HumanMessage  # noqa: E402

from brain.agents import orchestrator_config as oc  # noqa: E402
from brain.agents.nodes.helpers import build_turn_focus_directive  # noqa: E402

# ── Default DB-free per is_eligible (IDENTICI ai safe-default Rust) ──────────
DEFAULT_BEHAVIOR_MODES = ["automatico", "continuo"]
DEFAULT_INTENTS = [
    "code", "implement", "fix", "debug", "scaffold", "build", "refactor", "frontend",
]
DEFAULT_MIN_BUDGET = 2000


def _cfg_for(input_obj):
    """Costruisce il dict cfg che `orchestrator_config.get()` ritornerebbe, dai
    parametri del caso golden (DB-free)."""
    return {
        "plan_phase_enabled": input_obj.get("plan_phase_enabled", False),
        "plan_behavior_modes": input_obj.get("plan_behavior_modes", DEFAULT_BEHAVIOR_MODES),
        "plan_intents": input_obj.get("plan_intents", DEFAULT_INTENTS),
        "plan_min_token_budget": input_obj.get("plan_min_token_budget", DEFAULT_MIN_BUDGET),
    }


def _is_eligible(input_obj):
    """Chiama la VERA is_eligible monkeypatchando get() col cfg del caso."""
    cfg = _cfg_for(input_obj)
    orig = oc.get
    oc.get = lambda: cfg  # noqa: E731
    try:
        return bool(
            oc.is_eligible(
                input_obj.get("behavior_mode"),
                input_obj.get("intent"),
                int(input_obj.get("token_budget", 0) or 0),
            )
        )
    finally:
        oc.get = orig


def _plan_reuse(input_obj):
    """Replica byte-fedele della decisione di riuso piano (planner_node.py:84-100)."""
    existing = input_obj.get("existing")
    intent = input_obj.get("intent")
    behavior_mode = input_obj.get("behavior_mode")
    if existing is None:
        return "no_plan"
    plan_intent = existing.get("user_intent")
    plan_mode = existing.get("behavior_mode")
    intent_diverged = plan_intent is not None and plan_intent != intent
    mode_diverged = plan_mode is not None and plan_mode != behavior_mode
    if intent_diverged or mode_diverged:
        return "stale"
    return "reuse"


def _clarifying_branch(input_obj):
    """Replica byte-fedele del branching clarifying (planner_node.py:144-170)."""
    questions = input_obj.get("questions") or []
    behavior_mode = input_obj.get("behavior_mode")
    if not questions:
        return {"branch": "proceed"}
    is_confirm = behavior_mode in (None, "confirm", "study")
    if is_confirm:
        return {"branch": "halt", "questions": questions}
    # Applica default (Python costruisce `applied` ma il delta espone la lista
    # delle domande come applied_default_assumptions: `list(questions)`).
    return {"branch": "apply_defaults", "assumptions": list(questions)}


def _tool_catalog():
    """Replica 1:1 della costante nexus_todo_write (planner_node.py:221-290)."""
    return [
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
                                "node_key": {
                                    "type": "string",
                                    "description": "Comp.3a (DAG): chiave logica univoca del todo (es. 'schema_db', 'api', 'frontend'), per referenziarlo come dipendenza.",
                                },
                                "dep_keys": {
                                    "type": "array",
                                    "items": {"type": "string"},
                                    "description": "Comp.3a (DAG): node_key dei todo che devono COMPLETARSI prima di questo (dipendenze di esecuzione). Vuoto se indipendente.",
                                },
                            },
                            "required": ["content"],
                        },
                    },
                    "planner_model": {"type": "string"},
                    "rationale": {
                        "type": "string",
                        "description": "Razionale/strategia del piano: perche' questi todos in quest'ordine, assunzioni chiave.",
                    },
                    "constraints": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Vincoli/non-goal che hanno guidato il design del piano.",
                    },
                    "alternatives": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "option": {"type": "string"},
                                "rejected_because": {"type": "string"},
                            },
                        },
                        "description": "Approcci alternativi considerati e perche' scartati.",
                    },
                },
                "required": ["action", "run_id", "todos"],
            },
        }
    ]


def _build_hinted_system(input_obj):
    """Replica la concatenazione dei SOLI rami ON (planner_node.py:296-366):
    system_text + RUN_ID hint + <comprensione_preliminare> + turn_focus PREPEND.
    Usa il VERO build_turn_focus_directive."""
    system_text = input_obj.get("planner_system_text", "")
    run_id = input_obj.get("run_id", "")
    turn_focus_enabled = input_obj.get("turn_focus_enabled", True)

    # Messaggi LangChain (solo testo) per il turn_focus.
    messages = []
    for m in input_obj.get("messages") or []:
        role = m.get("role", "user")
        content = m.get("content", "")
        if role in ("assistant", "ai"):
            messages.append(AIMessage(content=content))
        else:
            messages.append(HumanMessage(content=content))

    hinted_system = (
        system_text
        + f"\n\nRUN_ID corrente: {run_id} (usalo come parametro run_id nel tool nexus_todo_write)"
    )
    _brief = str(input_obj.get("context_brief") or "").strip()
    if _brief:
        hinted_system += (
            "\n\n<comprensione_preliminare>\n"
            "Contesto raccolto prima di pianificare (grounding sul codebase + "
            "esplorazioni). Usalo per un piano fondato, non assunzioni alla cieca.\n"
            + _brief
            + "\n</comprensione_preliminare>"
        )
    # Rami OFF (RAG/backlog/dag_kb) non attraversati coi default.
    if turn_focus_enabled:
        _focus = build_turn_focus_directive(messages)
        if _focus:
            hinted_system = _focus + "\n\n" + hinted_system
    return hinted_system


def _build_tool_input(input_obj):
    """Replica build tool_input (planner_node.py:495-503)."""
    todo_block = input_obj.get("todo_block") or {}
    run_id = input_obj.get("run_id", "")
    used_provider = input_obj.get("used_provider", "")
    used_model = input_obj.get("used_model", "")
    intent = input_obj.get("intent")
    behavior_mode = input_obj.get("behavior_mode")
    tool_input = dict(todo_block.get("input") or {})
    tool_input["run_id"] = run_id
    tool_input.setdefault("planner_model", f"{used_provider}/{used_model}")
    if intent is not None:
        tool_input["user_intent"] = intent
    if behavior_mode is not None:
        tool_input["behavior_mode"] = behavior_mode
    return tool_input


def _parse_tool_result(input_obj):
    """Replica il gate result_obj.get('ok') (planner_node.py:517-527).
    Ritorna True (Ok) / False (Error)."""
    result_json = input_obj.get("result_json", "")
    try:
        result_obj = json.loads(result_json or "{}")
    except json.JSONDecodeError:
        result_obj = {"ok": False, "raw": result_json}
    return bool(result_obj.get("ok"))


def main():
    cases = []

    def add(function, input_obj, output):
        cases.append(
            {
                "case_id": f"{function}_{len([c for c in cases if c['function'] == function])}",
                "function": function,
                "input": input_obj,
                "output": output,
            }
        )

    # ── is_eligible (>=8 casi) ────────────────────────────────────────────────
    eligible_inputs = [
        {"plan_phase_enabled": True, "behavior_mode": "automatico", "intent": "code", "token_budget": 8000},
        {"plan_phase_enabled": True, "behavior_mode": "Automatico", "intent": "CODE", "token_budget": 8000},
        {"plan_phase_enabled": False, "behavior_mode": "automatico", "intent": "code", "token_budget": 8000},
        {"plan_phase_enabled": True, "behavior_mode": "confirm", "intent": "code", "token_budget": 8000},
        {"plan_phase_enabled": True, "behavior_mode": "automatico", "intent": "chat", "token_budget": 8000},
        {"plan_phase_enabled": True, "behavior_mode": "automatico", "intent": "code", "token_budget": 100},
        {"plan_phase_enabled": True, "behavior_mode": None, "intent": "code", "token_budget": 8000},
        {"plan_phase_enabled": True, "behavior_mode": "", "intent": "code", "token_budget": 8000},
        {"plan_phase_enabled": True, "behavior_mode": "continuo", "intent": "implement", "token_budget": 2000},
        {"plan_phase_enabled": True, "behavior_mode": "automatico", "intent": None, "token_budget": 8000},
    ]
    for inp in eligible_inputs:
        add("is_eligible", inp, _is_eligible(inp))

    # ── plan_reuse (>=5 casi) ─────────────────────────────────────────────────
    reuse_inputs = [
        {"existing": None, "intent": "code", "behavior_mode": "automatico"},
        {"existing": {"user_intent": None, "behavior_mode": None}, "intent": "code", "behavior_mode": "automatico"},
        {"existing": {"user_intent": "code", "behavior_mode": "automatico"}, "intent": "code", "behavior_mode": "automatico"},
        {"existing": {"user_intent": "docs", "behavior_mode": "automatico"}, "intent": "code", "behavior_mode": "automatico"},
        {"existing": {"user_intent": "code", "behavior_mode": "continuo"}, "intent": "code", "behavior_mode": "automatico"},
        {"existing": {"user_intent": "code"}, "intent": "code", "behavior_mode": "automatico"},
    ]
    for inp in reuse_inputs:
        add("plan_reuse", inp, _plan_reuse(inp))

    # ── clarifying_branch (>=5 casi) ──────────────────────────────────────────
    q1 = [{"id": "q1", "question": "Quale DB?", "suggested_default": "postgres"}]
    q2 = [
        {"id": "q1", "question": "Quale DB?", "suggested_default": "postgres"},
        {"id": "q2", "question": "Auth JWT o session?", "suggested_default": "jwt"},
    ]
    clar_inputs = [
        {"questions": [], "behavior_mode": "automatico"},
        {"questions": q1, "behavior_mode": "confirm"},
        {"questions": q1, "behavior_mode": "study"},
        {"questions": q1, "behavior_mode": None},
        {"questions": q1, "behavior_mode": "automatico"},
        {"questions": q2, "behavior_mode": "continuo"},
    ]
    for inp in clar_inputs:
        add("clarifying_branch", inp, _clarifying_branch(inp))

    # ── tool_catalog (1 caso, confronto strutturale) ──────────────────────────
    add("tool_catalog", {}, _tool_catalog())

    # ── build_hinted_system (>=5 casi, rami ON) ───────────────────────────────
    hinted_inputs = [
        {
            "planner_system_text": "Sei il planner.",
            "run_id": "RID-1",
            "turn_focus_enabled": True,
            "messages": [{"role": "user", "content": "Implementa il login"}],
        },
        {
            "planner_system_text": "Sei il planner.",
            "run_id": "RID-2",
            "turn_focus_enabled": False,
            "messages": [{"role": "user", "content": "Implementa il login"}],
        },
        {
            "planner_system_text": "Sei il planner.",
            "run_id": "RID-3",
            "turn_focus_enabled": True,
            "context_brief": "Il modulo auth usa JWT e bcrypt.",
            "messages": [
                {"role": "user", "content": "vecchia richiesta"},
                {"role": "assistant", "content": "fatto"},
                {"role": "user", "content": "ora crea index.html"},
            ],
        },
        {
            "planner_system_text": "Sei il planner.",
            "run_id": "RID-4",
            "turn_focus_enabled": True,
            "messages": [],  # nessun messaggio -> turn_focus vuoto
        },
        {
            "planner_system_text": "Sei il planner.",
            "run_id": "RID-5",
            "turn_focus_enabled": True,
            "context_brief": "   ",  # brief vuoto dopo strip -> non aggiunto
            "messages": [{"role": "user", "content": "Crea la dashboard"}],
        },
    ]
    for inp in hinted_inputs:
        add("build_hinted_system", inp, _build_hinted_system(inp))

    # ── build_tool_input (>=3 casi) ───────────────────────────────────────────
    ti_inputs = [
        {
            "todo_block": {"input": {"action": "create", "todos": [{"content": "X"}]}},
            "run_id": "RID", "used_provider": "anthropic", "used_model": "m1",
            "intent": "code", "behavior_mode": "automatico",
        },
        {
            "todo_block": {"input": {"planner_model": "gia-presente", "todos": []}},
            "run_id": "RID", "used_provider": "p", "used_model": "m",
            "intent": None, "behavior_mode": None,
        },
        {
            "todo_block": {},
            "run_id": "RID2", "used_provider": "openai", "used_model": "fast",
            "intent": "fix", "behavior_mode": "continuo",
        },
    ]
    for inp in ti_inputs:
        add("build_tool_input", inp, _build_tool_input(inp))

    # ── parse_tool_result (>=4 casi) ──────────────────────────────────────────
    ptr_inputs = [
        {"result_json": '{"ok": true}'},
        {"result_json": '{"ok": false}'},
        {"result_json": "{}"},
        {"result_json": "non-json"},
        {"result_json": ""},
    ]
    for inp in ptr_inputs:
        add("parse_tool_result", inp, _parse_tool_result(inp))

    out_path = "/tmp/golden_planner.json"
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(cases, f, ensure_ascii=False, indent=2)
    print(f"golden planner: scritti {len(cases)} casi in {out_path}")


if __name__ == "__main__":
    main()
