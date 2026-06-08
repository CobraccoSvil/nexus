"""Test del passback reasoning/thinking (guarded).

Verifica che il reasoning conservato in assistant_content venga ri-tradotto nel
formato API corretto, e che in ASSENZA di reasoning (caso attuale
disable_for_tools) il comportamento sia identico a prima (no-op, zero regressione).
"""
from __future__ import annotations

from brain.providers.adapter_base import convert_messages_to_openai


def _assistant_with_tool(reasoning: str | None):
    content = []
    if reasoning is not None:
        content.append({"type": "reasoning", "reasoning": reasoning})
    content.append({"type": "text", "text": "ok procedo"})
    content.append({"type": "tool_use", "id": "c1", "name": "write_file",
                    "input": {"path": "a.txt"}})
    return [{"role": "assistant", "content": content}]


def test_reasoning_passback_presente_diventa_reasoning_content():
    out = convert_messages_to_openai(_assistant_with_tool("penso quindi agisco"))
    assert len(out) == 1
    assert out[0]["reasoning_content"] == "penso quindi agisco"
    assert out[0]["tool_calls"][0]["function"]["name"] == "write_file"


def test_senza_reasoning_nessun_campo_reasoning_content():
    # Caso attuale (disable_for_tools): nessun blocco reasoning -> no-op.
    out = convert_messages_to_openai(_assistant_with_tool(None))
    assert len(out) == 1
    assert "reasoning_content" not in out[0]
    assert out[0]["tool_calls"][0]["function"]["name"] == "write_file"


def test_blocco_reasoning_senza_tool_non_rompe():
    # reasoning + solo testo (no tool): degrada al ramo testo, nessun crash.
    msgs = [{"role": "assistant", "content": [
        {"type": "reasoning", "reasoning": "r"},
        {"type": "text", "text": "risposta"},
    ]}]
    out = convert_messages_to_openai(msgs)
    assert out[0]["role"] == "assistant"
    assert "risposta" in out[0]["content"]


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
