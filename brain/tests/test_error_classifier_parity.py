"""Parita' cross-language del classificatore d'errore provider (regola L /
ADR 0026, Wave 8b).

Legge la stessa fixture letta dal test Rust
``provider_error_classifier.rs::parita_cross_language_da_fixture_golden``:
se entrambi passano, la classificazione testuale (subset stabile:
``stop_reason``) e' identica fra Python e Rust. Drift = bug.
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from brain.providers.error_handler import classify_error

FIXTURE = (
    Path(__file__).resolve().parents[2]
    / "tests"
    / "fixtures"
    / "error_classifier_golden.json"
)


# jscpd:ignore-start
# Boilerplate caricamento fixture: duplicazione GIUSTIFICATA col gemello
# test_text_chunk.py, il loro scopo e' essere simili (golden test).
def _load_cases():
    data = json.loads(FIXTURE.read_text(encoding="utf-8"))
    return data["cases"]


@pytest.mark.parametrize("case", _load_cases(), ids=lambda c: c["name"])
def test_parita_cross_language(case):
    res = classify_error(Exception(case["input"]), provider="test")
    assert res["stop_reason"] == case["expected_stop_reason"], (
        f"caso {case['name']!r}: stop_reason divergente "
        f"({res['stop_reason']} vs atteso {case['expected_stop_reason']})"
    )
# jscpd:ignore-end
