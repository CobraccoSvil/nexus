"""Test del passback thought_signature di Gemini (guarded).

Puri/offline: usano l'SDK google.genai solo per costruire types.Part localmente,
nessuna chiamata reale a Gemini. Verificano che:
- la firma opaca conservata in assistant_content (base64) venga riattaccata come
  bytes alla PRIMA functionCall part del turno (requisito Gemini 3);
- senza firma (config attuale disable_for_tools) il comportamento sia identico a
  prima (no-op, zero regressione);
- mai doppia firma tra Part; firma malformata ignorata senza crash.
"""
from __future__ import annotations

import base64

from brain.providers.google_provider import _convert_messages_to_google


def _asst(tool_sig_b64=None, with_tool=True, text_sig_b64=None):
    content = []
    tb = {"type": "text", "text": "ok"}
    if text_sig_b64:
        tb["thought_signature"] = text_sig_b64
    content.append(tb)
    if with_tool:
        b = {"type": "tool_use", "id": "c1", "name": "write_file", "input": {"path": "a.txt"}}
        if tool_sig_b64:
            b["thought_signature"] = tool_sig_b64
        content.append(b)
    return [{"role": "assistant", "content": content}]


def _function_call_parts(contents):
    return [p for c in contents for p in c.parts if getattr(p, "function_call", None)]


def test_signature_su_tool_use_riattaccata_come_bytes():
    raw = b"\x00\x01opaque"
    sig = base64.b64encode(raw).decode()
    out = _convert_messages_to_google(_asst(tool_sig_b64=sig))
    fc = _function_call_parts(out)
    assert len(fc) == 1
    assert fc[0].thought_signature == raw  # deserializzata esatta


def test_senza_signature_nessun_campo_noop():
    # Caso disable_for_tools: nessuna firma -> Part identica a oggi.
    out = _convert_messages_to_google(_asst(tool_sig_b64=None))
    fc = _function_call_parts(out)
    assert fc[0].thought_signature is None
    assert "thought_signature" not in fc[0].model_dump(exclude_none=True)


def test_signature_solo_sulla_prima_function_call():
    # Due tool_use con firma: solo la PRIMA la riceve (no doppia firma).
    sig = base64.b64encode(b"s").decode()
    content = [
        {"type": "tool_use", "id": "c1", "name": "t1", "input": {}, "thought_signature": sig},
        {"type": "tool_use", "id": "c2", "name": "t2", "input": {}, "thought_signature": sig},
    ]
    out = _convert_messages_to_google([{"role": "assistant", "content": content}])
    fc = _function_call_parts(out)
    assert len(fc) == 2
    sigs = [p.thought_signature for p in fc]
    assert sigs.count(b"s") == 1 and sigs.count(None) == 1


def test_signature_malformata_non_rompe():
    # base64 invalido -> degrada a Part senza firma, nessun crash.
    out = _convert_messages_to_google(_asst(tool_sig_b64="!!!non-base64!!!"))
    fc = _function_call_parts(out)
    assert fc[0].thought_signature is None


def test_signature_text_only_quando_no_tool():
    # Gemini 3 text-only: firma sull'ultima/unica part text.
    raw = b"txtsig"
    sig = base64.b64encode(raw).decode()
    out = _convert_messages_to_google(_asst(with_tool=False, text_sig_b64=sig))
    tparts = [p for c in out for p in c.parts if getattr(p, "text", None)]
    assert any(getattr(p, "thought_signature", None) == raw for p in tparts)


if __name__ == "__main__":
    import sys
    import traceback

    funcs = [v for k, v in sorted(globals().items())
             if k.startswith("test_") and callable(v)]
    failed = 0
    for fn in funcs:
        try:
            fn()
            print(f"PASS {fn.__name__}")
        except Exception:
            failed += 1
            print(f"FAIL {fn.__name__}")
            traceback.print_exc()
    print(f"\n{len(funcs) - failed}/{len(funcs)} test passati")
    sys.exit(1 if failed else 0)
