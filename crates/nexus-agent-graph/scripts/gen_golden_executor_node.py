#!/usr/bin/env python3
"""Golden di parita' 1:1 per la LOGICA DETERMINISTICA del SINGOLO turno di
`ExecutorNode` (`crates/nexus-agent-graph/src/nodes/executor.rs`), che porta
`brain/agents/nodes/__init__.py:executor_node` (1648-3513).

L'`executor_node` Python ha I/O profondo (chiamata provider, DB, summarizer):
NON e' una funzione pura isolata. Questo golden esercita la fetta DETERMINISTICA
del SINGOLO turno (la stessa che il nodo Rust assembla dai punti unici), con
LLM/store stubati fuori dalla scena:

  - gate di TESTA (early-return): superseded, declared-done>=3, G1 cap;
  - ordine NUDGE pre-LLM (esplorazione -> comando-fallito -> repeated_action ->
    resource_reallocation -> G1) e l'asse che scatta;
  - risoluzione provider/model (sticky > override > routing) + gate sentinella.

La logica e' embedded nel nodo (non funzioni isolate riusabili dal brain), quindi
la riproduciamo 1:1 qui — questo script E' la fonte di verita' del comportamento
Python osservabile, esattamente come gen_golden_executor_g1.py. Dove possibile
riusiamo i punti unici REALI del brain (progress_controller, i detector di
helpers), cosi' la parita' e' ancorata al codice di produzione.

Output: /tmp/golden_executor_node.json — lista di {group, case_id, input, output}.

Uso:
  python3 crates/nexus-agent-graph/scripts/gen_golden_executor_node.py
  cargo test -p nexus-agent-graph --lib golden_executor_node -- --ignored
"""
from __future__ import annotations

import json
import os
import sys

_REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
if _REPO_ROOT not in sys.path:
    sys.path.insert(0, _REPO_ROOT)


# ── Replica 1:1 dei gate di TESTA (early-return) ─────────────────────────────
#
# Le condizioni sono booleane/aritmetiche pure (i segnali derivati arrivano gia'
# risolti, come nel nodo Rust). Riproduciamo la PRIORITA' esatta: superseded ->
# declared-done>=3 -> G1 cap. L'output e' lo stop_reason emesso (o "proceed" se
# nessun gate di testa scatta e si prosegue ai nudge/LLM).


def head_gate(
    superseded: bool,
    declared_done: bool,
    declared_done_count: int,
    g1_cap_reached: bool,
) -> str:
    """Stop emesso dal primo gate di testa che scatta (py:1669-2042)."""
    if superseded:
        return "superseded"
    if declared_done and int(declared_done_count) >= 3:
        return "end_turn"  # chiusura d'autorita' su done ripetuto
    if g1_cap_reached:
        # Escalation = TODO (PR-J): senza candidato -> cap secco g1_cap_reached.
        return "g1_cap_reached"
    return "proceed"


# ── Ordine NUDGE pre-LLM: quale asse scatta e con quale azione ───────────────
#
# Riusa il PUNTO UNICO reale del brain (progress_controller.decide) per la
# decisione per-asse, replicando l'ORDINE di valutazione del nodo:
#   esplorazione(2x) -> comando-fallito -> repeated_action -> resource_reallocation
#   -> G1-descrittivo. Ritorna (axis, action) del primo asse che produce un nudge
#   o un abort; ("none","proceed") se nessuno.


def nudge_order(
    *,
    exploration_count: int,
    exploration_threshold: int,
    repeat_cmd_count: int,
    repeated_action: tuple | None,
    repeated_action_threshold: int,
    reallocation_count: int,
    reallocation_threshold: int,
    g1_descriptive: bool,
    already_guided: frozenset,
    progress_on: bool,
) -> dict:
    """Decisione del PRIMO asse di nudge che scatta (py:2093-2644)."""
    from brain.agents import progress_controller as pc

    # 1) Esplorazione a 2x soglia (guide/abort) — solo con controller ON.
    if progress_on and exploration_count >= 2 * exploration_threshold:
        dec = pc.decide(pc.ProgressSignals(
            exploration_count=exploration_count,
            exploration_threshold=exploration_threshold,
            already_guided=already_guided,
            has_escalation_candidate=False,
        ))
        return {"axis": "exploration", "action": dec.action}

    # 2) Comando ripetuto fallito (>=3): nudge testuale (non e' progress_ctrl).
    if repeat_cmd_count >= 3:
        return {"axis": "repeated_command", "action": "guide"}

    # 3) repeated_action — controller ON.
    if progress_on and repeated_action is not None:
        label, count = repeated_action
        if label and count >= repeated_action_threshold:
            dec = pc.decide(pc.ProgressSignals(
                repeated_action=(label, count),
                already_guided=already_guided,
                has_escalation_candidate=False,
            ))
            return {"axis": "repeated_action", "action": dec.action}

    # 4) resource_reallocation — controller ON.
    if progress_on and reallocation_count >= reallocation_threshold:
        dec = pc.decide(pc.ProgressSignals(
            reallocation_count=reallocation_count,
            reallocation_threshold=reallocation_threshold,
            already_guided=already_guided,
            has_escalation_candidate=False,
        ))
        return {"axis": "resource_reallocation", "action": dec.action}

    # 5) G1-descrittivo — controller ON (forza-azione hard).
    if progress_on and g1_descriptive:
        dec = pc.decide(pc.ProgressSignals(
            g1_over_cap=True,
            already_guided=already_guided,
        ))
        return {"axis": "g1_descriptive", "action": dec.action}

    return {"axis": "none", "action": "proceed"}


# ── Risoluzione provider/model: sticky > override > routing + sentinella ──────


def resolve_provider(
    *,
    sticky_provider: str | None,
    sticky_model: str | None,
    provider_override: str | None,
    model_override: str | None,
    routing_provider: str,
    routing_model: str,
) -> dict:
    """sticky > override > routing; sentinella -> no_provider (py:2460-2521)."""
    _SENT = ("__router_unavailable__", "__no_capable_provider__")
    prov = (sticky_provider or None) or (provider_override or None)
    modl = (sticky_model or None) or (model_override or None)
    if prov and modl:
        return {"provider": prov, "model": modl, "no_provider": False}
    p = prov or routing_provider
    m = modl or routing_model
    if (not p) or p in _SENT or m in _SENT:
        return {"provider": p, "model": m, "no_provider": True}
    return {"provider": p, "model": m, "no_provider": False}


def main() -> None:
    cases: list[dict] = []

    # ── head_gate ────────────────────────────────────────────────────────────
    head_inputs = [
        ("superseded", True, False, 0, False, "superseded"),
        ("done_3", False, True, 3, False, "end_turn"),
        ("done_2_non_chiude", False, True, 2, False, "proceed"),
        ("g1_cap", False, False, 0, True, "g1_cap_reached"),
        ("superseded_precede_done", True, True, 5, True, "superseded"),
        ("done_precede_g1", False, True, 4, True, "end_turn"),
        ("nessun_gate", False, False, 0, False, "proceed"),
    ]
    for (cid, sup, dd, ddc, cap, _exp) in head_inputs:
        cases.append({
            "group": "head_gate",
            "case_id": cid,
            "input": {
                "superseded": sup,
                "declared_done": dd,
                "declared_done_count": ddc,
                "g1_cap_reached": cap,
            },
            "output": head_gate(sup, dd, ddc, cap),
        })

    # ── nudge_order ──────────────────────────────────────────────────────────
    base = dict(
        exploration_count=0,
        exploration_threshold=6,
        repeat_cmd_count=0,
        repeated_action=None,
        repeated_action_threshold=2,
        reallocation_count=0,
        reallocation_threshold=3,
        g1_descriptive=False,
        already_guided=frozenset(),
        progress_on=True,
    )

    def add_nudge(cid: str, **over):
        args = dict(base)
        args.update(over)
        out = nudge_order(**args)
        # Serializza already_guided come lista ordinata per l'input JSON.
        in_json = dict(args)
        in_json["already_guided"] = sorted(args["already_guided"])
        cases.append({
            "group": "nudge_order",
            "case_id": cid,
            "input": in_json,
            "output": out,
        })

    add_nudge("nessuno")
    add_nudge("esplorazione_2x_guide", exploration_count=12)
    add_nudge("esplorazione_2x_abort", exploration_count=12,
              already_guided=frozenset({"exploration"}))
    add_nudge("comando_fallito", repeat_cmd_count=3)
    add_nudge("repeated_action_guide", repeated_action=("write_file: a.rs", 2))
    add_nudge("repeated_action_abort", repeated_action=("write_file: a.rs", 2),
              already_guided=frozenset({"repeated_action"}))
    add_nudge("reallocation_guide", reallocation_count=3)
    add_nudge("reallocation_abort", reallocation_count=3,
              already_guided=frozenset({"resource_reallocation"}))
    add_nudge("g1_descriptive_guide", g1_descriptive=True)
    # Priorita': esplorazione precede repeated_action.
    add_nudge("priorita_esplorazione", exploration_count=12,
              repeated_action=("x", 5))
    # Controller OFF: solo il comando-fallito (non progress_ctrl) puo' scattare.
    add_nudge("controller_off_repeated_ignorato", progress_on=False,
              repeated_action=("x", 5))
    add_nudge("controller_off_comando", progress_on=False, repeat_cmd_count=3)

    # ── resolve_provider ─────────────────────────────────────────────────────
    rp_inputs = [
        ("sticky_vince", "anthropic", "claude", "openai", "gpt", "google", "gemini"),
        ("override_se_no_sticky", None, None, "openai", "gpt", "google", "gemini"),
        ("routing_se_niente", None, None, None, None, "google", "gemini"),
        ("sentinella_routing", None, None, None, None, "__no_capable_provider__", "x"),
        ("vuoto_no_provider", None, None, None, None, "", ""),
        ("sticky_parziale_cade_su_routing", "anthropic", None, None, None, "google", "gemini"),
    ]
    for (cid, sp, sm, po, mo, rp, rm) in rp_inputs:
        cases.append({
            "group": "resolve_provider",
            "case_id": cid,
            "input": {
                "sticky_provider": sp,
                "sticky_model": sm,
                "provider_override": po,
                "model_override": mo,
                "routing_provider": rp,
                "routing_model": rm,
            },
            "output": resolve_provider(
                sticky_provider=sp, sticky_model=sm,
                provider_override=po, model_override=mo,
                routing_provider=rp, routing_model=rm,
            ),
        })

    out_path = "/tmp/golden_executor_node.json"
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(cases, f, ensure_ascii=False, indent=2)
    print(f"golden executor_node: {len(cases)} casi scritti in {out_path}")


if __name__ == "__main__":
    main()
