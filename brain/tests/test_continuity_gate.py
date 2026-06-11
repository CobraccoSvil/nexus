"""Test continuity gate semantico (mig 0397).

Eseguibile a mano: `PYTHONPATH=. python3 brain/tests/test_continuity_gate.py`.
"""
from __future__ import annotations

import sys

from langchain_core.messages import AIMessage, HumanMessage

from brain.agents.nodes import helpers as H


class _Vec:
    def __init__(self, values: list[float]) -> None:
        self.values = values


class _FakeEmb:
    """Embedding finto: 'db' -> asse X, 'figma' -> asse Y."""

    def embed_batch(self, model: str, texts: list[str]) -> list[_Vec]:
        out = []
        for t in texts:
            tl = t.lower()
            x = 1.0 if ("db" in tl or "tabelle" in tl) else 0.0
            y = 1.0 if "figma" in tl else 0.0
            out.append(_Vec([x, y, 0.1]))
        return out


def _mock_cfg(enabled: bool = True, min_score: float = 0.30, keep: int = 2):
    H._continuity_cfg_cache = {
        "enabled": enabled, "min_score": min_score, "keep_recent": keep,
    }
    import time
    H._continuity_cfg_ts = time.monotonic()


def test_cosine() -> None:
    assert abs(H._cosine([1, 0], [1, 0]) - 1.0) < 1e-9
    assert abs(H._cosine([1, 0], [0, 1])) < 1e-9
    assert H._cosine([0, 0], [1, 1]) == 0.0
    print("OK test_cosine")


def test_new_topic_trimma() -> None:
    _mock_cfg()
    msgs = [
        HumanMessage(content="implementa la app figma"),
        AIMessage(content="estratto il codice figma in app/"),
        HumanMessage(content="continua il lavoro figma"),
        AIMessage(content="fatto altro lavoro figma sui componenti"),
        HumanMessage(content="quante tabelle ci sono nel db"),
    ]
    out, score, trimmed = H.apply_continuity_trim(msgs, _FakeEmb())
    assert trimmed is True, f"score={score}"
    # puntatore + keep_recent(2) + query
    assert len(out) == 4, [type(m).__name__ for m in out]
    assert "nexus_search_semantic" in out[0].content
    assert out[-1].content == "quante tabelle ci sono nel db"
    print("OK test_new_topic_trimma")


def test_continuazione_non_trimma() -> None:
    _mock_cfg()
    msgs = [
        HumanMessage(content="implementa la app figma"),
        AIMessage(content="estratto il codice figma"),
        HumanMessage(content="lavora sul db del progetto"),
        AIMessage(content="ho creato le tabelle nel db"),
        HumanMessage(content="quante tabelle ci sono nel db"),
    ]
    out, score, trimmed = H.apply_continuity_trim(msgs, _FakeEmb())
    assert trimmed is False, f"score={score} deve essere >= soglia"
    assert out is msgs or len(out) == len(msgs)
    print("OK test_continuazione_non_trimma")


def test_fail_open() -> None:
    _mock_cfg()
    msgs = [HumanMessage(content="a"), AIMessage(content="b"),
            HumanMessage(content="c"), AIMessage(content="d"),
            HumanMessage(content="query")]
    # embeddings None -> niente trim
    out, score, trimmed = H.apply_continuity_trim(msgs, None)
    assert trimmed is False and score is None
    # disabled -> niente trim
    _mock_cfg(enabled=False)
    out2, _, trimmed2 = H.apply_continuity_trim(msgs, _FakeEmb())
    assert trimmed2 is False
    # history corta -> niente trim
    _mock_cfg()
    out3, _, trimmed3 = H.apply_continuity_trim(msgs[:2], _FakeEmb())
    assert trimmed3 is False
    print("OK test_fail_open")


def test_query_pulita_dai_blocchi_sistema() -> None:
    _mock_cfg()
    # Il blocco allegati_sessione cita figma: NON deve gonfiare la pertinenza.
    query = (
        "<allegati_sessione>\n- PL.make figma figma figma\n</allegati_sessione>\n\n"
        "quante tabelle ci sono nel db"
    )
    msgs = [
        HumanMessage(content="implementa la app figma"),
        AIMessage(content="lavoro figma"),
        HumanMessage(content="ancora figma"),
        AIMessage(content="altro figma"),
        HumanMessage(content=query),
    ]
    out, score, trimmed = H.apply_continuity_trim(msgs, _FakeEmb())
    assert trimmed is True, f"il blocco sistema non deve contare: score={score}"
    print("OK test_query_pulita_dai_blocchi_sistema")


if __name__ == "__main__":
    test_cosine()
    test_new_topic_trimma()
    test_continuazione_non_trimma()
    test_fail_open()
    test_query_pulita_dai_blocchi_sistema()
    print("\nTUTTI I TEST continuity_gate PASSATI")
    sys.exit(0)
