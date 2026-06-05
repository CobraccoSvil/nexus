"""Parita' cross-language del chunker (regola L / ADR 0026, Wave 8a).

Legge la stessa fixture letta dal test Rust ``chunker.rs::parita_cross_language_da_fixture_golden``: se entrambi passano, l'algoritmo
e' identico bit-per-bit fra Python e Rust. Drift = bug.
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from brain.utils.text_chunk import chunk_text

FIXTURE = Path(__file__).resolve().parents[2] / "tests" / "fixtures" / "chunker_golden.json"


# jscpd:ignore-start
# Boilerplate caricamento fixture: duplicazione GIUSTIFICATA col gemello
# test_error_classifier_parity.py, il loro scopo e' essere simili (golden test).
def _load_cases():
    data = json.loads(FIXTURE.read_text(encoding="utf-8"))
    return data["cases"]


@pytest.mark.parametrize("case", _load_cases(), ids=lambda c: c["name"])
def test_parita_cross_language(case):
    actual = chunk_text(case["input"], case["chunk_size"], case["overlap"])
    assert actual == case["expected"], (
        f"caso golden {case['name']!r} divergente fra Python e Rust: "
        f"size={case['chunk_size']} overlap={case['overlap']}"
    )
# jscpd:ignore-end
