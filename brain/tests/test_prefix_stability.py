"""Test P3 — stabilita' del prefix per il KV-cache.

Proprieta' verificate:
1. _compress_old_tool_results con cutoff_index FISSO produce output
   byte-identico sulla parte < cutoff anche quando la history cresce in coda
   (append-only), e NON tocca i messaggi >= cutoff.
2. _inject_forced_rag_reminder non tocca piu' il system e appende in coda.
3. _inject_language_reminder non muta piu' l'ultimo HumanMessage.

Eseguibile: PYTHONPATH=. python3 brain/tests/test_prefix_stability.py
"""
from __future__ import annotations

import sys

from langchain_core.messages import AIMessage, HumanMessage

from brain.agents.nodes import _compress_old_tool_results
from brain.agents.nodes import helpers as H


def _tool_msg(text: str) -> HumanMessage:
    return HumanMessage(
        content="",
        additional_kwargs={
            "anthropic_content": [
                {"type": "tool_result", "tool_use_id": "t", "content": text}
            ]
        },
    )


def _dump(msgs: list) -> str:
    out = []
    for m in msgs:
        ak = getattr(m, "additional_kwargs", {}) or {}
        out.append(repr((type(m).__name__, getattr(m, "content", ""), ak)))
    return "\n".join(out)


def test_cutoff_fisso_stabile_su_append() -> None:
    big = "X" * 5000
    base = [
        HumanMessage(content="task"),
        _tool_msg(big),
        _tool_msg(big),
        AIMessage(content="ok"),
    ]
    cutoff = 3  # comprimi solo i primi 3
    v1 = _compress_old_tool_results(list(base), max_content_chars=500, cutoff_index=cutoff)
    # La history cresce in coda (append-only): la parte < cutoff NON cambia.
    grown = list(base) + [_tool_msg(big), AIMessage(content="ancora")]
    v2 = _compress_old_tool_results(list(grown), max_content_chars=500, cutoff_index=cutoff)
    assert _dump(v1[:cutoff]) == _dump(v2[:cutoff]), "parte sotto cutoff deve essere byte-identica"
    # I messaggi >= cutoff sono INTATTI (anche i tool_result enormi).
    assert v2[4].additional_kwargs["anthropic_content"][0]["content"] == big, \
        "messaggi oltre il cutoff non vanno toccati"
    # E la parte sotto cutoff e' stata compressa davvero.
    c1 = v1[1].additional_kwargs["anthropic_content"][0]["content"]
    assert len(c1) < 5000 and "INDICIZZATI" in c1 or len(c1) < 5000
    print("OK test_cutoff_fisso_stabile_su_append")


def test_forced_rag_append_only() -> None:
    H._RAG_REMINDER_CACHE.update(
        {"loaded_at": __import__("time").time(), "threshold_ratio": 0.4, "text": "usa il RAG"}
    )
    msgs = [HumanMessage(content="a"), AIMessage(content="b")]
    system = "SYSTEM ORIGINALE"
    out_msgs, out_system = H._inject_forced_rag_reminder(msgs, system, est_tokens=900, window=1000)
    assert out_system == system, "il system NON va toccato (prefix stabile)"
    assert len(out_msgs) == 3 and H._RAG_REMINDER_MARKER in out_msgs[-1].content
    assert out_msgs[0] is msgs[0] and out_msgs[1] is msgs[1], "messaggi esistenti intatti"
    # Idempotente: seconda chiamata non duplica.
    out2, _ = H._inject_forced_rag_reminder(out_msgs, system, est_tokens=900, window=1000)
    assert len(out2) == 3
    print("OK test_forced_rag_append_only")


def test_language_reminder_non_muta_messaggi() -> None:
    msgs = [HumanMessage(content="domanda")]
    out_msgs, out_system = H._inject_language_reminder(msgs, "SYS", True, "solo italiano")
    assert out_msgs is msgs or out_msgs == msgs, "i messaggi non vanno piu' mutati"
    assert msgs[0].content == "domanda", "ultimo HumanMessage intatto"
    assert "solo italiano" in out_system, "garanzia nel system presente"
    print("OK test_language_reminder_non_muta_messaggi")


if __name__ == "__main__":
    test_cutoff_fisso_stabile_su_append()
    test_forced_rag_append_only()
    test_language_reminder_non_muta_messaggi()
    print("\nTUTTI I TEST prefix_stability PASSATI")
    sys.exit(0)
