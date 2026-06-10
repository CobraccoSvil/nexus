"""Test fix de-lessicalizzazione: _normalize_provider_result usa l'error_class
STRUTTURATO gia' nel metadata invece di ri-classificare la stringa (regola L,
elimina il fallback lessicale http_status su ogni errore provider).

Eseguibile a mano: `PYTHONPATH=. python3 brain/tests/test_normalize_provider_error.py`.
"""
from __future__ import annotations

import sys
import types

from brain.grpc_server import neural_service as ns


class _FakeResult:
    def __init__(self, content: str, metadata: dict) -> None:
        self.content = content
        self.metadata = metadata


def test_structured_error_class_no_reclassify() -> None:
    # Il provider ha gia' classificato sull'oggetto SDK (format_error_result):
    # metadata ha error_class + http_status. _normalize NON deve ri-classificare.
    called = {"reclassify": False}
    orig = ns._classify_provider_error

    def _spy(exc):  # pragma: no cover - non deve essere invocato
        called["reclassify"] = True
        return ("error", "ricalcolato")

    ns._classify_provider_error = _spy
    try:
        res = _FakeResult(
            content="",
            metadata={
                "error": "Crediti del provider esauriti.",
                "error_class": "billing_error",
                "http_status": 400,
            },
        )
        content, error_meta, error_class = ns._normalize_provider_result(res, "anthropic", "claude-x")
        assert error_class == "billing_error", error_class
        assert content == "Crediti del provider esauriti.", content
        assert called["reclassify"] is False, "non deve ri-classificare la stringa"
    finally:
        ns._classify_provider_error = orig
    print("OK test_structured_error_class_no_reclassify")


def test_legacy_error_falls_back_to_reclassify() -> None:
    # Path legacy "[Error: ...]" senza metadata strutturato: il fallback di
    # ri-classificazione resta attivo (comportamento legittimo).
    called = {"reclassify": False}
    orig = ns._classify_provider_error

    def _spy(exc):
        called["reclassify"] = True
        return ("rate_limit", "Troppe richieste.")

    ns._classify_provider_error = _spy
    try:
        res = _FakeResult(content="[Error: 429 too many requests]", metadata={})
        content, error_meta, error_class = ns._normalize_provider_result(res, "openai", "gpt")
        assert error_class == "rate_limit", error_class
        assert called["reclassify"] is True, "senza metadata strutturato deve ri-classificare"
    finally:
        ns._classify_provider_error = orig
    print("OK test_legacy_error_falls_back_to_reclassify")


def test_no_error_passthrough() -> None:
    # Nessun errore: content passa intatto, error_class vuoto.
    res = _FakeResult(content="risposta normale", metadata={})
    content, error_meta, error_class = ns._normalize_provider_result(res, "google", "gemini")
    assert content == "risposta normale"
    assert error_class == ""
    print("OK test_no_error_passthrough")


if __name__ == "__main__":
    test_structured_error_class_no_reclassify()
    test_legacy_error_falls_back_to_reclassify()
    test_no_error_passthrough()
    print("\nTUTTI I TEST normalize_provider_error PASSATI")
    sys.exit(0)
