"""Test del punto unico extract_cached_input_tokens (regola L).

Verifica la lettura dei cached prompt tokens dal formato OpenAI-compatible
(usage.prompt_tokens_details.cached_tokens) usato da openai e mistral, robusta
sia all'oggetto SDK (attributi) sia al dict grezzo.

Eseguibile: PYTHONPATH=. python3 brain/tests/test_cache_extract.py
"""
from __future__ import annotations

import sys
from types import SimpleNamespace

from brain.providers.adapter_base import extract_cached_input_tokens


def test_oggetto_sdk() -> None:
    usage = SimpleNamespace(
        prompt_tokens=19070,
        prompt_tokens_details=SimpleNamespace(cached_tokens=19008),
    )
    assert extract_cached_input_tokens(usage) == 19008
    print("OK test_oggetto_sdk")


def test_dict_grezzo() -> None:
    usage = {"prompt_tokens": 100, "prompt_tokens_details": {"cached_tokens": 64}}
    assert extract_cached_input_tokens(usage) == 64
    print("OK test_dict_grezzo")


def test_details_assenti() -> None:
    # Nessun campo cache (cache miss / provider che non lo riporta) -> 0.
    assert extract_cached_input_tokens(SimpleNamespace(prompt_tokens=100)) == 0
    assert extract_cached_input_tokens({"prompt_tokens": 100}) == 0
    print("OK test_details_assenti")


def test_cached_nullo_o_zero() -> None:
    # cached_tokens None o 0 -> 0 (non si popola cache_read_input_tokens).
    assert extract_cached_input_tokens(
        SimpleNamespace(prompt_tokens_details=SimpleNamespace(cached_tokens=None))
    ) == 0
    assert extract_cached_input_tokens(
        SimpleNamespace(prompt_tokens_details=SimpleNamespace(cached_tokens=0))
    ) == 0
    print("OK test_cached_nullo_o_zero")


if __name__ == "__main__":
    test_oggetto_sdk()
    test_dict_grezzo()
    test_details_assenti()
    test_cached_nullo_o_zero()
    print("\nTUTTI I TEST cache_extract PASSATI")
    sys.exit(0)
