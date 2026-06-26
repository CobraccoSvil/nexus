#!/usr/bin/env python3
"""Golden di parita' 1:1 per i 4 rami ON/seedati POST/PRE-LLM dell'`executor_node`
portati in PR-J2 (decisions::end_turn + executor.rs):

  - unfulfilled-report   (build_unfulfilled_report, helpers.py:1630-1685);
  - next_actions strip   (rimozione blocco <suggested_actions>, next_actions.py:262-295);
  - billing fail-fast    (messaggio onesto, __init__.py:2072-2079);
  - smart_upscale gate    (should_upscale + required, helpers.py:2755-2760).

Dove le funzioni reali del brain sono ISOLABILI senza dipendenze pesanti
(langchain/DB), le IMPORTIAMO e le esercitiamo direttamente (parita' ancorata al
codice di produzione). Dove l'import del modulo trascinerebbe langchain_core/DB
(helpers.py, next_actions.py via brain.agents.__init__), REPLICHIAMO la logica
DETERMINISTICA 1:1 qui (stesso pattern di gen_golden_executor_node.py): questo
script E' la fonte di verita' del comportamento Python osservabile per la fetta
deterministica.

Output: /tmp/golden_executor_end_turn.json — lista di {group, case_id, input, output}.

Uso:
  python3 crates/nexus-agent-graph/scripts/gen_golden_executor_end_turn.py
  cargo test -p nexus-agent-graph --lib golden_executor_end_turn -- --ignored
"""
from __future__ import annotations

import json
import os
import re
import sys

_REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
if _REPO_ROOT not in sys.path:
    sys.path.insert(0, _REPO_ROOT)


# ── (1) build_unfulfilled_report (helpers.py:1630-1685) ──────────────────────
#
# Deterministico: conta i tool_use (con file path) dai blocchi della history,
# sintetizza il resoconto onesto. Replicato 1:1 (helpers.py importa langchain/DB
# al top, l'import diretto e' fragile in ambiente isolato).


def build_unfulfilled_report(result_text, messages):
    tool_counts: dict[str, int] = {}
    files_touched: list[str] = []
    for m in messages or []:
        content = m.get("content")
        if not isinstance(content, list):
            continue
        for block in content:
            if not isinstance(block, dict):
                continue
            if block.get("type") == "tool_use":
                name = str(block.get("name") or "tool")
                tool_counts[name] = tool_counts.get(name, 0) + 1
                inp = block.get("input")
                if isinstance(inp, dict):
                    path = inp.get("path") or inp.get("file_path") or inp.get("filename")
                    if isinstance(path, str) and path and path not in files_touched:
                        files_touched.append(path)
    lines: list[str] = [
        "Mi sono fermato annunciando un'attesa o un passo successivo senza "
        "eseguirlo, quindi il compito NON e' completato. Ecco il resoconto onesto:",
        "",
    ]
    if tool_counts:
        azioni = ", ".join(
            f"{n}x {name}" for name, n in sorted(tool_counts.items(), key=lambda kv: -kv[1])
        )
        lines.append(f"- Cosa ho fatto: {azioni}.")
    else:
        lines.append("- Cosa ho fatto: nessuna azione concreta in questo turno.")
    if files_touched:
        shown = ", ".join(files_touched[:12])
        more = "" if len(files_touched) <= 12 else f" (+{len(files_touched) - 12} altri)"
        lines.append(f"- File toccati: {shown}{more}.")
    snippet = (result_text or "").strip().replace("\n", " ")
    if snippet:
        lines.append(f'- Dove mi sono interrotto: "{snippet[-180:]}"')
    lines.append(
        "- Cosa manca: portare a termine il compito; l'ultimo passo annunciato "
        "non e' stato eseguito."
    )
    lines.append(
        "- Prossimo passo proposto: invece di attendere passivamente, diagnosticare "
        "lo stato reale (es. leggere i log del servizio/container che non parte) e "
        "agire sulla causa. Confermi se procedo?"
    )
    return "\n".join(lines)


# ── (2) strip <suggested_actions> (next_actions.py:262-295) ──────────────────
#
# Regex IGNORECASE|DOTALL, non-greedy + rstrip finale. Replicato 1:1 (importare
# next_actions trascina brain.agents.__init__).
_BLOCK_RE = re.compile(
    r"<suggested_actions>\s*(.*?)\s*</suggested_actions>",
    re.IGNORECASE | re.DOTALL,
)


def strip_suggested_actions(text: str) -> str:
    if not text or "<suggested_actions>" not in text.lower():
        return text or ""
    cleaned = _BLOCK_RE.sub("", text).rstrip()
    return cleaned


# ── (3) billing fail-fast message (__init__.py:2075-2079) ────────────────────


def billing_fail_fast_message(exploration_count, exploration_threshold, exhausted):
    if int(exploration_count) < int(exploration_threshold):
        return None
    if not exhausted:
        return None
    return (
        f"L'elaborazione si e' interrotta: i provider AI principali sono in "
        f"cooldown per quota/credito esaurito ({', '.join(exhausted)}). "
        f"Ricarica i crediti (o attendi il reset) e riprova."
    )


# ── (4) smart_upscale gate (helpers.py:2755-2760) ────────────────────────────


def should_upscale(enabled, est_tokens, current_window) -> bool:
    if not enabled or int(est_tokens) <= 0 or int(current_window) <= 0:
        return False
    return int(est_tokens) >= current_window * 0.9


def upscale_required(est_tokens, overhead) -> int:
    return int(int(est_tokens) * float(overhead))


def main() -> None:
    cases: list[dict] = []

    # ── unfulfilled-report ────────────────────────────────────────────────────
    def tu(name, path=None):
        blk = {"type": "tool_use", "name": name}
        if path is not None:
            blk["input"] = {"path": path}
        return {"content": [blk]}

    report_inputs = [
        (
            "due_write_un_read",
            "Ora attendo il build.",
            [tu("write_file", "a.rs"), tu("write_file", "b.rs"), tu("read_file", "a.rs")],
        ),
        ("nessuna_azione", "Procedo.", []),
        ("solo_tool_no_file", "Verifico.", [tu("run_command")]),
        ("result_vuoto", "", [tu("write_file", "x.ts")]),
    ]
    for cid, rt, msgs in report_inputs:
        cases.append({
            "group": "unfulfilled_report",
            "case_id": cid,
            "input": {"result_text": rt, "messages": msgs},
            "output": build_unfulfilled_report(rt, msgs),
        })

    # ── strip_suggested_actions ───────────────────────────────────────────────
    strip_inputs = [
        ("blocco_semplice", "Risposta.\n<suggested_actions>\n[{\"label\":\"x\"}]\n</suggested_actions>"),
        ("nessun_blocco", "Solo testo finale."),
        ("blocco_inline", "Prima <suggested_actions>[]</suggested_actions> dopo"),
        ("blocco_case", "A <SUGGESTED_ACTIONS>x</SUGGESTED_ACTIONS> B"),
        ("blocco_senza_chiusura", "Testo <suggested_actions> mai chiuso"),
    ]
    for cid, text in strip_inputs:
        cases.append({
            "group": "strip_suggested_actions",
            "case_id": cid,
            "input": {"text": text},
            "output": strip_suggested_actions(text),
        })

    # ── billing fail-fast ─────────────────────────────────────────────────────
    billing_inputs = [
        ("scatta_due", 6, 6, ["anthropic", "openai"]),
        ("scatta_uno", 7, 6, ["deepseek"]),
        ("sotto_soglia", 5, 6, ["anthropic"]),
        ("nessun_esausto", 6, 6, []),
    ]
    for cid, cnt, thr, ex in billing_inputs:
        out = billing_fail_fast_message(cnt, thr, ex)
        cases.append({
            "group": "billing_fail_fast",
            "case_id": cid,
            "input": {"exploration_count": cnt, "exploration_threshold": thr, "exhausted": ex},
            "output": out,  # None oppure stringa
        })

    # ── smart_upscale gate ────────────────────────────────────────────────────
    upscale_inputs = [
        ("a_soglia", True, 90000, 100000),
        ("sopra", True, 95000, 100000),
        ("sotto", True, 89999, 100000),
        ("disabilitato", False, 95000, 100000),
        ("window_ignota", True, 95000, 0),
        ("est_zero", True, 0, 100000),
    ]
    for cid, en, est, win in upscale_inputs:
        cases.append({
            "group": "should_upscale",
            "case_id": cid,
            "input": {"enabled": en, "est_tokens": est, "current_window": win},
            "output": should_upscale(en, est, win),
        })
    # required tokens
    for cid, est, ov in [("std", 100000, 1.2), ("ratio_2", 50000, 2.0)]:
        cases.append({
            "group": "upscale_required",
            "case_id": cid,
            "input": {"est_tokens": est, "overhead": ov},
            "output": upscale_required(est, ov),
        })

    out_path = "/tmp/golden_executor_end_turn.json"
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(cases, f, ensure_ascii=False, indent=2)
    print(f"golden executor_end_turn: {len(cases)} casi scritti in {out_path}")


if __name__ == "__main__":
    main()
