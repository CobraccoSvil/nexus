#!/usr/bin/env python3
"""Golden di parita' 1:1 per `decisions::g1_accounting` (CONTEGGIO del gate G1
re-entry/cap dell'executor) del crate `nexus-agent-graph`.

Replica il SOLO conteggio del blocco G1 di `brain/agents/nodes/__init__.py`
(executor_node, ~1882-2042):

  is_reentry = (
      iterations >= 1
      and prev_stop_reason in ("end_turn", "stop")
      and not has_pending
      and (action_oriented or unfulfilled)
      and not recent_error
  )
  updated_count = current_count + (1 if is_reentry else 0)
  cap_reached   = updated_count >= max_nudges

Questa logica e' aritmetica/booleana pura (i 3 segnali derivati turn_action_oriented /
unfulfilled / recent_error arrivano gia' risolti: NON sono ricalcolati qui). Non c'e'
nulla da importare dal brain per il conteggio in se' (e' embedded nel nodo, non una
funzione isolata): la riproduciamo 1:1 nello script, che e' la fonte di verita' del
comportamento Python osservabile.

Output: /tmp/golden_executor_g1.json — lista di {group, case_id, input, output}.

Uso:
  python3 crates/nexus-agent-graph/scripts/gen_golden_executor_g1.py
  cargo test -p nexus-agent-graph --lib golden_executor_g1 -- --ignored
"""
from __future__ import annotations

import json
import os
import sys

_REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
if _REPO_ROOT not in sys.path:
    sys.path.insert(0, _REPO_ROOT)


def g1_accounting(
    prev_stop_reason,
    iterations: int,
    has_pending: bool,
    action_oriented: bool,
    unfulfilled: bool,
    recent_error: bool,
    current_count: int,
    max_nudges: int,
):
    """Replica 1:1 del conteggio G1 del blocco executor_node."""
    is_reentry = (
        int(iterations) >= 1
        and prev_stop_reason in ("end_turn", "stop")
        and not has_pending
        and (action_oriented or unfulfilled)
        and not recent_error
    )
    updated_count = int(current_count) + (1 if is_reentry else 0)
    cap_reached = updated_count >= int(max_nudges)
    return {
        "updated_count": updated_count,
        "is_reentry": is_reentry,
        "cap_reached": cap_reached,
    }


def main() -> None:
    cases = []
    # (case_id, prev_stop, iters, pending, action, unfulfilled, recent_err, cur, max)
    inputs = [
        ("reentry_base",        "end_turn", 3, False, True,  False, False, 0, 3),
        ("reentry_stop",        "stop",     1, False, True,  False, False, 0, 3),
        ("stop_none",           None,       3, False, True,  False, False, 0, 3),
        ("stop_tool_use",       "tool_use", 3, False, True,  False, False, 0, 3),
        ("iter_zero",           "end_turn", 0, False, True,  False, False, 0, 3),
        ("has_pending",         "end_turn", 3, True,  True,  False, False, 0, 3),
        ("recent_error",        "end_turn", 3, False, True,  False, True,  0, 3),
        ("unfulfilled_only",    "end_turn", 2, False, False, True,  False, 0, 3),
        ("ne_action_ne_unful",  "end_turn", 2, False, False, False, False, 0, 3),
        ("cap_al_terzo",        "end_turn", 5, False, True,  False, False, 2, 3),
        ("cap_gia_superato",    None,       5, False, True,  False, False, 3, 3),
        ("max_diverso",         "end_turn", 4, False, True,  False, False, 4, 5),
        ("max_uno_subito_cap",  "end_turn", 1, False, True,  False, False, 0, 1),
        ("recent_err_e_unful",  "stop",     2, False, False, True,  True,  1, 3),
    ]
    for (cid, prev, iters, pend, act, unf, rerr, cur, mx) in inputs:
        out = g1_accounting(prev, iters, pend, act, unf, rerr, cur, mx)
        cases.append({
            "group": "g1_accounting",
            "case_id": cid,
            "input": {
                "prev_stop_reason": prev,
                "iterations": iters,
                "has_pending": pend,
                "action_oriented": act,
                "unfulfilled": unf,
                "recent_error": rerr,
                "current_count": cur,
                "max_nudges": mx,
            },
            "output": out,
        })

    out_path = "/tmp/golden_executor_g1.json"
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(cases, f, ensure_ascii=False, indent=2)
    print(f"golden executor_g1: {len(cases)} casi scritti in {out_path}")


if __name__ == "__main__":
    main()
