#!/usr/bin/env python3
"""Genera il golden di parita' 1:1 per il FinalGateNode Rust.

Importa le funzioni DETERMINISTICHE reali da brain/agents/final_gate.py
(`_is_software_task`, `_count_build_errors`, `_render_failed_block`,
`route_after_final_gate`) e replica byte-fedele la DECISION MACHINE di
`final_gate_node` (deterministica una volta noti i risultati dei criteri, che
sono INPUT stubati: il golden NON esegue criteria_runner).

Output: /tmp/golden_final_gate.json — lista di {case_id, function, input, output}
consumata dal test Rust `golden::golden_final_gate_parita`.

Nessun accesso al DB: `_is_software_task` viene esercitato monkeypatchando
`has_filesystem_mutation_in_history` con una versione DB-free basata sul CSV
mutator di default (identico a _FS_MUTATORS_DEFAULT), e passando un `cfg`
esplicito con la whitelist software_intents = ai default RoutingConfig Rust.

Uso:
  python3 crates/nexus-agent-graph/scripts/gen_golden_final_gate.py
  cargo test -p nexus-agent-graph --lib golden_final_gate_parita -- --ignored
"""
from __future__ import annotations

import json
import os
import sys

# Rende importabile il package `brain` dalla root del repo.
_REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
if _REPO_ROOT not in sys.path:
    sys.path.insert(0, _REPO_ROOT)

from brain.agents import final_gate as fg  # noqa: E402

# Whitelist intent IDENTICA ai _SAFE_DEFAULTS Rust (RoutingConfig::default).
SOFTWARE_INTENTS = [
    "code", "debug", "scaffold", "implement", "build", "frontend", "fix", "refactor",
]
# CSV mutator IDENTICO a _FS_MUTATORS_DEFAULT (helpers.py / config.rs / mig 0394).
FS_MUTATORS = set(
    s.strip()
    for s in (
        "write_file,edit_file,delete_file,rename_file,file_write,fs_copy,fs_mkdir,"
        "fs_move,format_file,run_lint_fix,run_command,command,run_in_terminal,"
        "git_command,git_pull,git_commit,git_stage,git_push,nexus_extract_figma_code,"
        "nexus_install_shadcn_components,nexus_mcp_tool_call,cargo_install,run_service,"
        "service_restart,stop_service"
    ).split(",")
    if s.strip()
)

FINAL_GATE_MAX_CYCLES = 2


def _mutation_db_free(messages):
    """Replica has_filesystem_mutation_in_history SENZA DB: i messaggi del golden
    portano i tool_use in un campo strutturato `tool_uses` (lista di nomi). Usa il
    CSV mutator di default (cache 60s DB-driven nel runtime, qui costante)."""
    for m in messages:
        if not isinstance(m, dict):
            continue
        if m.get("role") not in ("assistant", "ai"):
            continue
        for name in m.get("tool_uses") or []:
            if name in FS_MUTATORS:
                return True
    return False


# Monkeypatch dell'import dinamico dentro _is_software_task: la funzione fa
# `from .nodes.helpers import has_filesystem_mutation_in_history`; sostituiamo il
# simbolo nel modulo helpers cosi' la versione DB-free viene risolta.
import brain.agents.nodes.helpers as _helpers  # noqa: E402

_helpers.has_filesystem_mutation_in_history = _mutation_db_free


def _cfg():
    return {
        "final_gate_software_intents": SOFTWARE_INTENTS,
        "final_gate_max_cycles": FINAL_GATE_MAX_CYCLES,
    }


def _decision_machine(state, results, max_cycles):
    """Replica byte-fedele la decision machine di final_gate_node DATI i risultati
    criteri (gate enabled+software gia' decisi a monte). Ritorna il delta come
    dict (o {} mai qui: i casi di pass-through sono testati a parte via
    is_software_task)."""
    cycle = int(state.get("final_gate_cycle", 0) or 0) + 1
    passed = all(r["passed"] for r in results)
    if passed:
        return {
            "final_gate_cycle": 0,
            "stop_reason": "end_turn",
            "final_gate_passed": True,
        }
    forced = bool(state.get("forced_close_unverified"))
    if forced or cycle >= max_cycles:
        return {"final_gate_cycle": 0, "stop_reason": "end_turn"}
    block = fg._render_failed_block(state, cycle, max_cycles, results)
    return {
        "messages": [block],
        "final_gate_cycle": cycle,
        "stop_reason": "tool_use",
        "pending_tool_uses": [],
    }


def main() -> None:
    cases: list[dict] = []

    def add(case_id, function, inp, out):
        cases.append({"case_id": case_id, "function": function, "input": inp, "output": out})

    # ── is_software_task: mutazione strutturale, whitelist intent, nessuno ──────
    cfg = _cfg()
    sw_cases = [
        ("sw_mutation_write", {"messages": [{"role": "assistant", "tool_uses": ["write_file"]}]}),
        ("sw_mutation_rename", {"messages": [{"role": "assistant", "tool_uses": ["rename_file"]}],
                                "user_intent": "architecture"}),  # fuori whitelist ma muta -> True
        ("sw_intent_code", {"user_intent": "code"}),
        ("sw_intent_debug", {"user_intent": "debug"}),
        ("sw_intent_fallback_extra", {"intent": "frontend"}),
        ("sw_explore_only", {"messages": [{"role": "assistant", "tool_uses": ["read_file"]}],
                             "user_intent": "chat"}),  # esplora + non-software -> False
        ("sw_none", {}),
        ("sw_intent_chat", {"user_intent": "chat"}),
        ("sw_intent_uppercase", {"user_intent": "CODE"}),  # lower-cased -> True
    ]
    for cid, inp in sw_cases:
        state = {
            "messages": inp.get("messages") or [],
            "user_intent": inp.get("user_intent"),
            "intent": inp.get("intent"),
        }
        # _is_software_task usa messages/user_intent/intent + cfg; passiamo
        # messages come lista di dict (la versione DB-free li interpreta).
        # _is_software_task interno legge state["messages"] e le passa a
        # has_filesystem_mutation_in_history (la nostra versione DB-free).
        out = fg._is_software_task(state, cfg)
        add(cid, "is_software_task", inp, out)

    # ── count_build_errors: TS / rustc / syntax / generico / nessuno ────────────
    cb_cases = [
        ("cb_empty", ""),
        ("cb_none_match", "build completato senza problemi"),
        ("cb_ts_two", "src/a.ts(1,1): error TS2304: x\nb.ts: error TS1005: ;"),
        ("cb_rustc", "error[E0432]: unresolved import `foo`"),
        ("cb_syntax_type", "SyntaxError: unexpected token\nTypeError: undefined"),
        ("cb_generico", "error: cannot find crate `serde`"),
        ("cb_mix", "error TS2322: t\nerror[E0277]: u\nerror: linker failed"),
    ]
    for cid, out_str in cb_cases:
        add(cid, "count_build_errors", {"output": out_str}, fg._count_build_errors(out_str))

    # ── render_failed_block: build con/ senza troncamento, non-build, autonomy ──
    rfb_cases = [
        ("rfb_build_trunc", {
            "behavior_mode": "bilanciata", "cycle": 1, "max_cycles": 2,
            "results": [{
                "type": "run_command", "passed": False,
                "evidence": {
                    "output_excerpt": "error TS2304: a\nerror TS2305: b",
                    "exit_code": 1, "output_total_chars": 9999, "output_truncated": True,
                },
            }],
        }),
        ("rfb_build_no_trunc", {
            "behavior_mode": "", "cycle": 2, "max_cycles": 3,
            "results": [{
                "type": "run_command", "passed": False,
                "evidence": {
                    "output_excerpt": "error[E0432]: x",
                    "exit_code": 1, "output_total_chars": 15, "output_truncated": False,
                },
            }],
        }),
        ("rfb_non_build_verdict", {
            "behavior_mode": "automatico", "cycle": 1, "max_cycles": 2,
            "results": [{
                "type": "service_logs_clean", "passed": False,
                "evidence": {"verdict": "errore 500 nei log del servizio"},
            }],
        }),
        ("rfb_non_build_error", {
            "behavior_mode": "continuous", "cycle": 1, "max_cycles": 2,
            "results": [{
                "type": "no_orphan_imported", "passed": False,
                "evidence": {"error": "hello-world non raggiunge il design"},
            }],
        }),
        ("rfb_no_excerpt", {
            "behavior_mode": "", "cycle": 2, "max_cycles": 2,
            "results": [{"type": "outputs_exist", "passed": False, "evidence": {}}],
        }),
        # FIX A: output_excerpt = "" (falsy) -> la catena `or` cade su verdict.
        # Python rende il verdict; il Rust con la vecchia chain (Some("") che
        # interrompe) avrebbe saltato il criterio (excerpt vuoto -> continue).
        ("rfb_excerpt_empty_falls_to_verdict", {
            "behavior_mode": "", "cycle": 1, "max_cycles": 2,
            "results": [{
                "type": "service_logs_clean", "passed": False,
                "evidence": {"output_excerpt": "", "verdict": "errore 500 dal verdict"},
            }],
        }),
        # FIX B: output_total_chars = 0 (falsy) -> `or len(text)` cade su len(text).
        # output_truncated True forza l'header che stampa total_chars: con lo 0
        # tenuto (vecchio Rust) sarebbe "(.../0 char)", col fallback e' len(text).
        ("rfb_build_total_chars_zero", {
            "behavior_mode": "", "cycle": 1, "max_cycles": 2,
            "results": [{
                "type": "run_command", "passed": False,
                "evidence": {
                    "output_excerpt": "error TS2304: x",
                    "exit_code": 1, "output_total_chars": 0, "output_truncated": True,
                },
            }],
        }),
        ("rfb_multi", {
            "behavior_mode": "continuo", "cycle": 1, "max_cycles": 2,
            "results": [
                {"type": "no_orphan_imported", "passed": False,
                 "evidence": {"verdict": "placeholder"}},
                {"type": "run_command", "passed": False,
                 "evidence": {"output_excerpt": "error: boom", "exit_code": 1,
                              "output_total_chars": 11, "output_truncated": False}},
            ],
        }),
    ]
    for cid, inp in rfb_cases:
        state = {"behavior_mode": inp["behavior_mode"]}
        out = fg._render_failed_block(state, inp["cycle"], inp["max_cycles"], inp["results"])
        add(cid, "render_failed_block", inp, out)

    # ── route_after_final_gate: tool_use -> executor, altro -> learner ──────────
    for cid, sr in [
        ("route_tool_use", "tool_use"),
        ("route_end_turn", "end_turn"),
        ("route_none", None),
        ("route_other", "stop"),
    ]:
        state = {"stop_reason": sr} if sr is not None else {}
        add(cid, "route_after_final_gate", {"stop_reason": sr},
            fg.route_after_final_gate(state))

    # ── decision_machine: passed / forced / cap / fail (varie cycle/flag) ───────
    dm_cases = [
        ("dm_passed", {
            "final_gate_cycle": 0, "max_cycles": 2,
            "results": [{"type": "no_orphan_imported", "passed": True, "evidence": {}},
                        {"type": "outputs_exist", "passed": True, "evidence": {}}],
        }),
        ("dm_forced", {
            "final_gate_cycle": 0, "max_cycles": 2, "forced_close_unverified": True,
            "results": [{"type": "no_orphan_imported", "passed": False,
                         "evidence": {"verdict": "fail"}}],
        }),
        ("dm_cap", {
            "final_gate_cycle": 1, "max_cycles": 2,
            "results": [{"type": "no_orphan_imported", "passed": False,
                         "evidence": {"verdict": "fail"}}],
        }),
        ("dm_fail_reexec", {
            "final_gate_cycle": 0, "max_cycles": 2, "behavior_mode": "bilanciata",
            "results": [{"type": "no_orphan_imported", "passed": False,
                         "evidence": {"verdict": "hello-world placeholder"}}],
        }),
        ("dm_fail_reexec_autonomous", {
            "final_gate_cycle": 0, "max_cycles": 3, "behavior_mode": "automatico",
            "results": [{"type": "run_command", "passed": False,
                         "evidence": {"output_excerpt": "error TS2304: x", "exit_code": 1,
                                      "output_total_chars": 14, "output_truncated": False}}],
        }),
    ]
    for cid, inp in dm_cases:
        state = {
            "final_gate_cycle": inp.get("final_gate_cycle", 0),
            "forced_close_unverified": inp.get("forced_close_unverified", False),
            "behavior_mode": inp.get("behavior_mode", ""),
        }
        out = _decision_machine(state, inp["results"], inp["max_cycles"])
        add(cid, "decision_machine", inp, out)

    out_path = "/tmp/golden_final_gate.json"
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(cases, f, ensure_ascii=False, indent=2)
    print(f"golden final_gate: {len(cases)} casi scritti in {out_path}")


if __name__ == "__main__":
    main()
