#!/usr/bin/env python3
"""Golden di parita' 1:1 per `decisions::loop_signatures` (signature anti-loop +
exploration counter) del crate `nexus-agent-graph`.

Replica la logica di `brain/agents/nodes/__init__.py` (`executor_node`, blocchi
~3135-3317) e di `brain/agents/nodes/helpers.py` (`_EXPLORATION_ONLY_TOOLS`):

  - build_signature(name, input):
        sig_input = json.dumps(input or {}, sort_keys=True, ensure_ascii=False)
        sig = f"{name}|{hashlib.sha1(sig_input.encode()).hexdigest()[:12]}"
  - detect_signature_loop(recent, new): LOOP_THRESHOLD=3, finestra combined[-6:],
        updated_signatures=(recent+new)[-12:]
  - exploration_counter_update(pending_names, count, nudge): se tutte esplorative
        count += len, altrimenti reset count=0/nudge=False.

La COSTRUZIONE della signature usa il codice REALE `json.dumps`/`hashlib.sha1` di
Python (non c'e' nulla da importare dal brain: e' stdlib), quindi il golden e'
intrinsecamente bit-fedele. Per la lista `_EXPLORATION_ONLY_TOOLS` si tenta
l'import REALE da helpers.py; se non disponibile si usa la replica 1:1 versionata
qui sotto (allineata a helpers.py:517-542).

Output: /tmp/golden_executor_signals.json (lista di {group, case_id, input, output})
consumata dal test Rust `decisions::loop_signatures::golden::golden_executor_signals`.

Uso:
  python3 crates/nexus-agent-graph/scripts/gen_golden_executor_signals.py
  cargo test -p nexus-agent-graph --lib golden_executor_signals -- --ignored
"""
from __future__ import annotations

import hashlib
import json
import os
import sys

_REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
if _REPO_ROOT not in sys.path:
    sys.path.insert(0, _REPO_ROOT)

# ── _EXPLORATION_ONLY_TOOLS: import REALE con fallback 1:1 ───────────────────
try:
    from brain.agents.nodes.helpers import _EXPLORATION_ONLY_TOOLS as _EXPL  # type: ignore

    EXPLORATION_ONLY_TOOLS = set(_EXPL)
except Exception as _exc:  # noqa: BLE001
    print(f"[gen_golden_executor_signals] import brain non disponibile ({_exc}); uso replica 1:1")
    EXPLORATION_ONLY_TOOLS = {
        "nexus_list_archive_entries", "nexus_read_archive_entry",
        "nexus_inspect_attachment", "nexus_extract_figma_structure",
        "nexus_list_attachments", "nexus_read_attachment",
        "nexus_extract_docx_text", "nexus_extract_xlsx_data",
        "nexus_extract_pdf_text", "nexus_describe_image_attachment",
        "read_file",
        "list_files", "grep", "read_file_lines", "search_in_files",
        "nexus_mcp_tool_search",
        "nexus_get_worklog",
    }

LOOP_THRESHOLD = 3


# ── Replica byte-fedele della logica executor_node ──────────────────────────
def build_signature(name: str, tool_input) -> str:
    sig_input = json.dumps(tool_input or {}, sort_keys=True, ensure_ascii=False)
    return f"{name}|{hashlib.sha1(sig_input.encode()).hexdigest()[:12]}"


def detect_signature_loop(recent: list, new_signatures: list):
    combined = list(recent) + list(new_signatures)
    loop_sig = None
    if len(combined) >= LOOP_THRESHOLD and new_signatures:
        for sig in new_signatures:
            tail = [s for s in combined[-LOOP_THRESHOLD * 2:] if s == sig]
            if len(tail) >= LOOP_THRESHOLD:
                loop_sig = sig
                break
    updated = (list(recent) + list(new_signatures))[-12:]
    return loop_sig, updated


def exploration_counter_update(pending_names: list, count: int, nudge: bool):
    if not pending_names:
        return count, nudge, False
    all_expl = all(n in EXPLORATION_ONLY_TOOLS for n in pending_names)
    if all_expl:
        return count + len(pending_names), nudge, False
    return 0, False, True


def main() -> None:
    cases = []

    # ── Gruppo 1: build_signature (canonicalizzazione CRUCIALE) ──────────────
    sig_inputs = [
        ("empty", "read_file", {}),
        ("null_input", "read_file", None),
        ("simple", "read_file", {"path": "src/main.rs"}),
        # Ordine chiavi sparso: sort_keys=True deve renderlo irrilevante.
        ("key_order_a", "edit_file", {"path": "a.rs", "offset": 1, "content": "x"}),
        ("key_order_b", "edit_file", {"offset": 1, "content": "x", "path": "a.rs"}),
        # Nested + ordine sparso anche all'interno.
        ("nested", "run_command", {"opts": {"z": 1, "a": 2}, "command": "ls"}),
        ("nested_reorder", "run_command", {"command": "ls", "opts": {"a": 2, "z": 1}}),
        # Unicode non-ASCII (ensure_ascii=False -> letterale).
        ("unicode", "write_file", {"path": "città.txt", "content": "caffè è buono"}),
        # Tipi misti: bool/int/float/null/array.
        ("mixed_types", "tool_x", {"flag": True, "n": 42, "f": 1.5, "nil": None, "arr": [3, 1, 2]}),
        # Array di oggetti con chiavi sparse.
        ("array_objs", "tool_y", {"items": [{"b": 1, "a": 2}, {"d": 3, "c": 4}]}),
        # Stringa con caratteri da escapare (virgolette, backslash, newline).
        ("escapes", "run_command", {"command": "echo \"ciao\"\n\tfine \\ end"}),
        # FLOAT esponenziali: json.dumps di Python usa float.__repr__ (formato +
        # tie-break round-half-to-even). La serializzazione Rust DEVE coincidere
        # bit-a-bit o la signature anti-loop diverge dal brain (cfr. py_json.rs
        # python_float_repr). Senza il fix questi casi fallirebbero.
        ("float_small", "tool_f", {"small": 1e-6}),
        ("float_threshold", "tool_f", {"threshold": 1e-7}),
        ("float_e5", "tool_f", {"x": 1e-5, "y": 1.23e-5}),
        ("float_big", "tool_f", {"big": 6.022e23}),
        ("float_n16", "tool_f", {"n": 1e16}),
        ("float_mix", "tool_f", {"mix": [1.0, 0.5, 100.0, 1e-8]}),
        # Tie-break round-half-to-even (la stdlib Rust diverge: ...43.13).
        ("float_tie_even", "tool_f", {"v": -111275153569243.125}),
    ]
    for case_id, name, inp in sig_inputs:
        out = build_signature(name, inp)
        cases.append({
            "group": "build_signature",
            "case_id": case_id,
            "input": {"name": name, "tool_input": inp},
            "output": out,
        })

    # ── Gruppo 2: detect_signature_loop ──────────────────────────────────────
    sa = build_signature("read_file", {"path": "x"})
    sb = build_signature("grep", {"q": "y"})
    loop_inputs = [
        # Sotto threshold (2 occorrenze) -> nessun loop.
        ("two_no_loop", [sa], [sa]),
        # Tre occorrenze nella finestra -> loop su sa.
        ("three_loop", [sa, sb, sa], [sa]),
        # new vuoto -> nessun loop anche se combined>=3.
        ("empty_new", [sa, sa, sa], []),
        # combined<3 -> nessun loop.
        ("short_combined", [], [sa]),
        # Loop ma il sig in loop e' la seconda new (ordine di emissione).
        ("loop_second_new", [sb, sb], [sa, sb]),
        # Cap 12: recent lunga, tiene le ultime 12.
        ("cap_twelve", [f"s|{i:02x}" for i in range(15)], ["s|new"]),
        # Finestra -6: occorrenze fuori finestra non contano.
        ("outside_window", [sa, sa, "x|1", "x|2", "x|3", "x|4"], [sa]),
    ]
    for case_id, recent, new in loop_inputs:
        loop_sig, updated = detect_signature_loop(recent, new)
        cases.append({
            "group": "detect_signature_loop",
            "case_id": case_id,
            "input": {"recent": recent, "new_signatures": new},
            "output": {"loop_signature": loop_sig, "updated_signatures": updated},
        })

    # ── Gruppo 3: exploration_counter_update ─────────────────────────────────
    expl_inputs = [
        ("empty_textual", [], 4, True),          # turno testuale -> invariato
        ("all_exploration", ["read_file", "grep"], 3, False),  # 3+2=5
        ("one_productive", ["read_file", "write_file"], 5, True),  # reset
        ("single_explore", ["nexus_read_attachment"], 0, False),  # 0+1=1
        ("single_productive", ["run_command"], 7, True),  # reset
        ("unknown_tool_productive", ["tool_sconosciuto"], 2, False),  # non esplorativo -> reset
    ]
    for case_id, names, count, nudge in expl_inputs:
        c2, n2, reset = exploration_counter_update(names, count, nudge)
        cases.append({
            "group": "exploration_counter_update",
            "case_id": case_id,
            "input": {
                "pending_tool_names": names,
                "current_count": count,
                "current_nudge_sent": nudge,
            },
            "output": {
                "consecutive_exploration_calls": c2,
                "exploration_nudge_sent": n2,
                "reset_exploration_axis": reset,
            },
        })

    out_path = "/tmp/golden_executor_signals.json"
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(cases, f, ensure_ascii=False, indent=2)
    print(f"golden executor_signals: {len(cases)} casi scritti in {out_path}")


if __name__ == "__main__":
    main()
