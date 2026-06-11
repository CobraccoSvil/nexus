"""Parita' cross-language dell'estrazione JSON da output LLM (regola L /
ADR 0026 / ADR 0032, Wave 6).

Legge la stessa fixture letta dal test Rust
``llm_json.rs::parita_cross_language_da_fixture_golden``: se entrambi passano,
l'estrazione (3 strategie: fence-strip + parse diretto, brace-matching con
stato stringhe/escape, fallback regex single-level) e' identica fra
``brain/utils/json_extract.py`` e ``crates/mcp-core/src/llm_json.rs``.
Comportamento target: il Python attuale. Drift = bug.
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from brain.utils.json_extract import extract_json_block

FIXTURE = (
    Path(__file__).resolve().parents[2]
    / "tests"
    / "fixtures"
    / "json_extract_golden.json"
)


# jscpd:ignore-start
# Boilerplate caricamento fixture: duplicazione GIUSTIFICATA coi gemelli
# test_text_chunk.py / test_error_classifier_parity.py, il loro scopo e'
# essere simili (golden test).
def _load_cases():
    data = json.loads(FIXTURE.read_text(encoding="utf-8"))
    return data["cases"]


@pytest.mark.parametrize("case", _load_cases(), ids=lambda c: c["name"])
def test_parita_cross_language(case):
    actual = extract_json_block(case["input"])
    assert actual == case["expected"], (
        f"caso {case['name']!r}: estrazione divergente "
        f"({actual!r} vs atteso {case['expected']!r})"
    )
# jscpd:ignore-end
