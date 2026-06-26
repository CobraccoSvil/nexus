#!/usr/bin/env python3
"""Golden di parita' 1:1 per `decisions::tool_dispatch` e `decisions::predictive_cap`
del crate `nexus-agent-graph`.

Importa le funzioni REALI dal brain quando disponibili (fonte di verita'), con
fallback a una replica 1:1 versionata qui se l'import non e' possibile (ambiente
senza deps brain):

  brain/agents/nodes/helpers.py:
    - apply_run_notes(current, tool_input)
    - normalize_declared_outcome(tool_input)
    - _estimate_tool_result_size_bytes(tool_name, args)
    - _extract_returned_bytes(result_content)
    - _estimate_context_chars(messages)            (su oggetti dict-like, no DB)
    - _current_context_token_estimate(messages, system_text)
    - PREDICTIVE_CAP_SENTINEL  + il CALCOLO di _predictive_cap_check (parte pura)
  brain/agents/todo_reminder.py:
    - append_reminder_block(blocks, reminder_text)

Le funzioni di stima contesto Python iterano `m.content` / `m.additional_kwargs`.
Qui usiamo un piccolo shim `FakeMsg` con quegli attributi cosi' la funzione REALE
del brain gira senza DB ne' LangChain. Per _predictive_cap_check, che fa IO (lookup
DB del context_window), riproduciamo SOLO la parte aritmetica/di formattazione pura
(la stessa che il modulo Rust espone come funzione pura) usando il SENTINEL reale.

Output: /tmp/golden_dispatch_pure.json — lista di {group, case_id, input, output}.

Uso:
  python3 crates/nexus-agent-graph/scripts/gen_golden_dispatch_pure.py
  cargo test -p nexus-agent-graph --lib golden_dispatch_pure -- --ignored
"""
from __future__ import annotations

import json
import os
import sys

_REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
if _REPO_ROOT not in sys.path:
    sys.path.insert(0, _REPO_ROOT)

RUN_NOTES_MAX_CHARS = 2400
TOKEN_DIV = 3.5
PREDICTIVE_CAP_SENTINEL = "[ERROR: chiamata bloccata da predictive context cap]"

# ── Import REALE con fallback 1:1 ────────────────────────────────────────────
try:
    from brain.agents.nodes.helpers import (  # type: ignore
        apply_run_notes,
        normalize_declared_outcome,
        _estimate_tool_result_size_bytes,
        _extract_returned_bytes,
        _estimate_context_chars,
        _current_context_token_estimate,
        RUN_NOTES_MAX_CHARS as _RNMC,
        PREDICTIVE_CAP_SENTINEL as _SENT,
    )
    from brain.agents.todo_reminder import append_reminder_block  # type: ignore

    RUN_NOTES_MAX_CHARS = _RNMC
    PREDICTIVE_CAP_SENTINEL = _SENT
    _USING_REAL = True
except Exception as _exc:  # noqa: BLE001
    print(f"[gen_golden_dispatch_pure] import brain non disponibile ({_exc}); uso replica 1:1")
    _USING_REAL = False

    _VALID_OUTCOMES = frozenset({"done", "blocked", "needs_input"})

    def apply_run_notes(current, tool_input):
        if not isinstance(tool_input, dict):
            return None
        action = str(tool_input.get("action", "")).strip().lower()
        content = str(tool_input.get("content", "")).strip()
        if action not in ("set", "append") or not content:
            return None
        if action == "set":
            notes = content
        else:
            notes = ((current or "").rstrip() + "\n" + content).strip() if current else content
        if len(notes) > RUN_NOTES_MAX_CHARS:
            notes = "[...]\n" + notes[-(RUN_NOTES_MAX_CHARS - 6):]
        return notes

    def normalize_declared_outcome(tool_input):
        if not isinstance(tool_input, dict):
            return None
        outcome = str(tool_input.get("outcome", "")).strip().lower()
        if outcome not in _VALID_OUTCOMES:
            return None
        out = {"outcome": outcome, "summary": str(tool_input.get("summary", "")).strip()}
        for k in ("next_step", "blocked_by"):
            v = tool_input.get(k)
            if v:
                out[k] = str(v).strip()
        return out

    def _estimate_tool_result_size_bytes(tool_name, args):
        if not isinstance(args, dict):
            args = {}
        if tool_name in ("nexus_read_attachment", "nexus_read_archive_entry"):
            length = args.get("length")
            try:
                length_i = int(length) if length is not None else 102_400
            except Exception:
                length_i = 102_400
            encoding = str(args.get("encoding", "auto") or "auto").lower()
            overhead = 1.4 if encoding in ("auto", "base64") else 1.05
            return int(length_i * overhead)
        if tool_name == "nexus_extract_pdf_text":
            return 100_000
        if tool_name in ("nexus_extract_docx_text", "nexus_extract_xlsx_data", "nexus_extract_figma_structure"):
            return 80_000
        if tool_name in ("nexus_list_archive_entries", "nexus_list_attachments", "nexus_inspect_attachment"):
            return 4_000
        if tool_name == "nexus_describe_image_attachment":
            return 8_000
        return 5_000

    def _extract_returned_bytes(result_content):
        try:
            data = json.loads(result_content) if result_content else {}
            if isinstance(data, dict):
                v = data.get("length")
                if isinstance(v, int):
                    return max(0, v)
        except Exception:
            pass
        return 0

    def _estimate_context_chars(messages):
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

    def _current_context_token_estimate(messages, system_text=""):
        total_chars = len(system_text or "")
        for m in messages:
            c = getattr(m, "content", "")
            if isinstance(c, str):
                total_chars += len(c)
            elif isinstance(c, list):
                for b in c:
                    if isinstance(b, dict):
                        for v in b.values():
                            if isinstance(v, str):
                                total_chars += len(v)
            extra = getattr(m, "additional_kwargs", {}) or {}
            anth = extra.get("anthropic_content")
            if isinstance(anth, list):
                for b in anth:
                    if isinstance(b, dict):
                        for v in b.values():
                            if isinstance(v, str):
                                total_chars += len(v)
            elif isinstance(anth, str):
                total_chars += len(anth)
        return int(total_chars / 3.5)

    def append_reminder_block(anthropic_content_blocks, reminder_text):
        if not reminder_text:
            return
        anthropic_content_blocks.append({
            "type": "text",
            "text": f"<system-reminder>\n{reminder_text}\n</system-reminder>",
        })


# Shim BaseMessage-like per le stime contesto (no LangChain, no DB).
class FakeMsg:
    def __init__(self, content="", anthropic_content=None):
        self.content = content
        self.additional_kwargs = {}
        if anthropic_content is not None:
            self.additional_kwargs["anthropic_content"] = anthropic_content


def _predictive_cap_pure(ratio, window, expected_bytes, current_tokens):
    """Parte PURA di _predictive_cap_check (senza IO): stesso calcolo e stesso testo."""
    cap_tokens = int(window * ratio)
    expected_tokens = int(expected_bytes / 3.5)
    projected = current_tokens + expected_tokens
    if projected <= cap_tokens:
        return None
    pct = int(current_tokens / max(window, 1) * 100)
    return (
        PREDICTIVE_CAP_SENTINEL + "\n"
        "ATTENZIONE: e' stata bloccata SOLO questa chiamata, NON il task. "
        "Se questo tool non e' essenziale per la RICHIESTA CORRENTE dell'utente "
        "(es. l'hai chiamato per via di contenuti storici della conversazione), "
        "IGNORALO e prosegui col task usando i dati che hai gia' raccolto. "
        "NON dichiarare il task bloccato per questo motivo.\n"
        f"Dettaglio: context a {current_tokens} token ({pct}% del budget {window}); il "
        f"risultato atteso aggiungerebbe ~{expected_tokens} token oltre il "
        f"{int(ratio*100)}% (cap={cap_tokens}).\n"
        "Solo se il tool e' DAVVERO necessario alla richiesta corrente:\n"
        "- Riduci i parametri (es. length piu' piccolo).\n"
        "- Usa estrazione strutturata (nexus_extract_figma_structure, "
        "nexus_extract_pdf_text, nexus_extract_docx_text).\n"
        "- Oppure dichiara con task_complete outcome=needs_input cosa serve dall'utente."
    )


def main() -> None:
    cases = []

    # ── apply_run_notes ──────────────────────────────────────────────────────
    big = "a" * (RUN_NOTES_MAX_CHARS + 100)
    run_notes_inputs = [
        ("set", "vecchio", {"action": "set", "content": " nuovo "}),
        ("append", "riga1", {"action": "append", "content": "riga2"}),
        ("append_none", None, {"action": "append", "content": "primo"}),
        ("invalid_action", None, {"action": "x", "content": "y"}),
        ("empty_content", "x", {"action": "set", "content": "   "}),
        ("not_dict", None, "non oggetto"),
        ("cap_tail", None, {"action": "set", "content": big}),
        ("append_su_lungo", "b" * (RUN_NOTES_MAX_CHARS - 3), {"action": "append", "content": "coda"}),
    ]
    for cid, cur, inp in run_notes_inputs:
        out = apply_run_notes(cur, inp if isinstance(inp, dict) else inp)
        cases.append({
            "group": "apply_run_notes", "case_id": cid,
            "input": {"current": cur, "tool_input": inp},
            "output": out,
        })

    # ── normalize_declared_outcome ───────────────────────────────────────────
    outcome_inputs = [
        ("done_full", {"outcome": "DONE", "summary": " fatto ", "next_step": "", "blocked_by": " dep "}),
        ("blocked", {"outcome": "blocked", "summary": "x", "blocked_by": "manca dep"}),
        ("needs_input_next", {"outcome": "needs_input", "next_step": "chiedi url"}),
        ("no_summary", {"outcome": "done"}),
        ("invalid", {"outcome": "fatto"}),
        ("not_dict", [1, 2]),
    ]
    for cid, inp in outcome_inputs:
        out = normalize_declared_outcome(inp)
        cases.append({
            "group": "normalize_declared_outcome", "case_id": cid,
            "input": {"tool_input": inp}, "output": out,
        })

    # ── _estimate_tool_result_size_bytes ─────────────────────────────────────
    size_inputs = [
        ("read_base64", "nexus_read_attachment", {"length": 1000}),
        ("read_text", "nexus_read_attachment", {"length": 1000, "encoding": "text"}),
        ("read_auto_explicit", "nexus_read_attachment", {"length": 2000, "encoding": "auto"}),
        ("read_default_len", "nexus_read_archive_entry", {}),
        ("read_str_len", "nexus_read_attachment", {"length": "500"}),
        ("read_bad_len", "nexus_read_attachment", {"length": "abc"}),
        ("pdf", "nexus_extract_pdf_text", {}),
        ("docx", "nexus_extract_docx_text", {}),
        ("figma", "nexus_extract_figma_structure", {}),
        ("list", "nexus_list_attachments", {}),
        ("inspect", "nexus_inspect_attachment", {}),
        ("image", "nexus_describe_image_attachment", {}),
        ("default", "tool_qualunque", {}),
    ]
    for cid, tn, args in size_inputs:
        out = _estimate_tool_result_size_bytes(tn, args)
        cases.append({
            "group": "estimate_tool_result_size_bytes", "case_id": cid,
            "input": {"tool_name": tn, "args": args}, "output": out,
        })

    # ── _extract_returned_bytes ──────────────────────────────────────────────
    bytes_inputs = [
        ("ok", '{"length": 512}'),
        ("negative", '{"length": -5}'),
        ("no_length", '{"other": 1}'),
        ("not_json", "non json"),
        ("empty", ""),
        ("float_len", '{"length": 12.5}'),
    ]
    for cid, content in bytes_inputs:
        out = _extract_returned_bytes(content)
        cases.append({
            "group": "extract_returned_bytes", "case_id": cid,
            "input": {"result_content": content}, "output": out,
        })

    # ── _estimate_context_chars / _current_context_token_estimate ────────────
    # I messaggi sono descritti come spec {content, anthropic_content}: lo script li
    # costruisce con FakeMsg, il test Rust con ContextMessage. STESSA forma.
    msg_specs = [
        ("only_str", [{"content": "ciao", "anthropic_content": None}], ""),
        ("str_plus_block_content",
         [{"content": "ciao", "anthropic_content": [{"type": "text", "content": "AB"}]}], ""),
        ("content_list_ignored_in_chars",
         [{"content": ["lista"], "anthropic_content": [{"type": "text", "content": "AB"}, {"type": "tool_use", "name": "x"}]}], ""),
        ("token_with_system",
         [{"content": "abcdefg", "anthropic_content": [{"type": "text", "text": "xyz"}]}], "sys"),
        ("token_content_list",
         [{"content": [{"type": "text", "text": "hello"}, {"type": "tool_use", "name": "edit", "id": "c1"}], "anthropic_content": None}], ""),
        ("anth_string",
         [{"content": "base", "anthropic_content": "stringa_anth"}], ""),
        ("multi",
         [
             {"content": "primo", "anthropic_content": None},
             {"content": "secondo", "anthropic_content": [{"type": "text", "content": "blk", "text": "T"}]},
         ], "S"),
    ]
    for cid, specs, system in msg_specs:
        msgs = [FakeMsg(s.get("content", ""), s.get("anthropic_content")) for s in specs]
        chars = _estimate_context_chars(msgs)
        tokens = _current_context_token_estimate(msgs, system)
        cases.append({
            "group": "estimate_context_chars", "case_id": cid,
            "input": {"messages": specs}, "output": chars,
        })
        cases.append({
            "group": "current_context_token_estimate", "case_id": cid,
            "input": {"messages": specs, "system_text": system}, "output": tokens,
        })

    # ── append_reminder_block ────────────────────────────────────────────────
    reminder_inputs = [
        ("empty_noop", [{"type": "text", "text": "x"}], ""),
        ("appends", [{"type": "text", "text": "x"}], "ricorda i todo"),
        ("from_empty", [], "solo questo"),
    ]
    for cid, blocks, text in reminder_inputs:
        b = [dict(x) for x in blocks]
        append_reminder_block(b, text)
        cases.append({
            "group": "append_reminder_block", "case_id": cid,
            "input": {"blocks": blocks, "reminder_text": text}, "output": b,
        })

    # ── predictive_cap_check (parte pura) ────────────────────────────────────
    cap_inputs = [
        ("sotto_soglia", 0.8, 100_000, 3500, 1000),
        ("sopra_soglia", 0.8, 100_000, 350_000, 79_000),
        ("al_cap", 0.5, 100, 35, 40),
        ("appena_sopra", 0.5, 100, 36, 40),
        ("window_grande", 0.85, 200_000, 700_000, 150_000),
    ]
    for cid, ratio, window, exp_bytes, cur_tok in cap_inputs:
        out = _predictive_cap_pure(ratio, window, exp_bytes, cur_tok)
        cases.append({
            "group": "predictive_cap_check", "case_id": cid,
            "input": {
                "ratio": ratio, "window": window,
                "expected_size_bytes": exp_bytes, "current_tokens": cur_tok,
            },
            "output": out,
        })

    cases.append({
        "group": "predictive_cap_sentinel", "case_id": "literal",
        "input": {}, "output": PREDICTIVE_CAP_SENTINEL,
    })

    out_path = "/tmp/golden_dispatch_pure.json"
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(cases, f, ensure_ascii=False, indent=2)
    print(f"golden dispatch_pure: {len(cases)} casi scritti in {out_path} (real_import={_USING_REAL})")


if __name__ == "__main__":
    main()
