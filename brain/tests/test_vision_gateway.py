"""Test degli endpoint vision (brain/grpc_server/routes/vision.py) SENZA rete.

Verifica che /vision/describe e /vision/compare passino ora dal GatewayProvider
(non dagli SDK vendor diretti): il provider/model viene risolto via purpose
(nexus_purpose_model), i messaggi sono costruiti con un blocco image_url (data
URI) e la chiamata e' delegata a GatewayProvider.generate_agent_turn. L'output
(description/ocr_text, similarity_score/differences) resta identico.

Idempotenti, auto-contenuti: nessun DB, nessun gateway reale. Il routing
singleton e il GatewayProvider sono mockati.
"""
from __future__ import annotations

import base64
from types import SimpleNamespace
from typing import Any

import pytest

from brain.grpc_server.routes import vision
from brain.providers.base import ProviderResult


def _png_b64() -> str:
    """Un base64 valido (byte arbitrari, non un PNG reale: gli endpoint non
    validano la struttura immagine, solo il mime e la decodificabilita')."""
    return base64.b64encode(b"\x89PNG\r\n\x1a\n_fake_pixels_").decode("ascii")


class _FakeRoutingClient:
    """Singleton di routing fittizio: ritorna un provider/model fisso."""

    def __init__(self, provider: str, model: str) -> None:
        self._provider = provider
        self._model = model

    def purpose_model(self, *, purpose: str) -> SimpleNamespace:  # noqa: ARG002
        return SimpleNamespace(provider=self._provider, model=self._model)


def _patch_routing(monkeypatch: pytest.MonkeyPatch, provider: str, model: str) -> None:
    from brain.router import service as router_service

    monkeypatch.setattr(
        router_service,
        "_routing_client_singleton",
        lambda: _FakeRoutingClient(provider, model),
    )


def _patch_gateway(
    monkeypatch: pytest.MonkeyPatch, result: ProviderResult, captured: dict[str, Any]
) -> None:
    """Sostituisce GatewayProvider.generate_agent_turn catturando gli argomenti."""
    from brain.providers import gateway_provider

    async def _fake_turn(self: Any, **kwargs: Any) -> ProviderResult:  # noqa: ARG001
        captured.update(kwargs)
        return result

    monkeypatch.setattr(
        gateway_provider.GatewayProvider, "generate_agent_turn", _fake_turn
    )


@pytest.mark.asyncio
async def test_describe_via_gateway_costruisce_image_url(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """describe passa dal gateway: model pinnato provider/model, messaggio con
    blocco image_url data URI; output description/ocr parsato dal formato."""
    _patch_routing(monkeypatch, "google", "gemini-x")
    captured: dict[str, Any] = {}
    _patch_gateway(
        monkeypatch,
        ProviderResult(
            provider="google",
            model="gemini-x",
            content="DESCRIZIONE: un grafico a barre\nOCR: Fatturato 2026",
            metadata={"stop_reason": "end_turn"},
        ),
        captured,
    )

    body = vision.VisionDescribeRequest(image_base64=_png_b64(), mime_type="image/png")
    out = await vision.vision_describe(body)

    # Output invariato (stesso parser DESCRIZIONE/OCR).
    assert out["description"] == "un grafico a barre"
    assert out["ocr_text"] == "Fatturato 2026"
    assert out["model_used"] == "google/gemini-x"

    # Il gateway e' stato chiamato col model pinnato e il blocco image_url.
    assert captured["model"] == "google/gemini-x"
    assert captured["tools"] == []
    user_content = captured["messages"][0]["content"]
    assert user_content[0]["type"] == "text"
    img = user_content[1]
    assert img["type"] == "image_url"
    assert img["image_url"]["url"].startswith("data:image/png;base64,")


@pytest.mark.asyncio
async def test_describe_errore_gateway_diventa_502(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Un ProviderResult d'errore del gateway -> HTTP 502 (no fallback nascosto)."""
    from fastapi import HTTPException

    _patch_routing(monkeypatch, "anthropic", "claude-x")
    captured: dict[str, Any] = {}
    _patch_gateway(
        monkeypatch,
        ProviderResult(
            provider="anthropic",
            model="claude-x",
            content="[Error: billing]",
            metadata={"stop_reason": "error", "error": "billing_error"},
        ),
        captured,
    )

    body = vision.VisionDescribeRequest(image_base64=_png_b64(), mime_type="image/png")
    with pytest.raises(HTTPException) as exc:
        await vision.vision_describe(body)
    assert exc.value.status_code == 502


@pytest.mark.asyncio
async def test_describe_503_se_purpose_non_configurato(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Purpose non configurato (provider __no_model__) -> 503, niente gateway."""
    from fastapi import HTTPException

    _patch_routing(monkeypatch, "__no_model__", "__no_model__")
    body = vision.VisionDescribeRequest(image_base64=_png_b64(), mime_type="image/png")
    with pytest.raises(HTTPException) as exc:
        await vision.vision_describe(body)
    assert exc.value.status_code == 503


@pytest.mark.asyncio
async def test_compare_via_gateway_due_immagini(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """compare passa dal gateway con DUE blocchi image_url (screenshot poi
    reference); output similarity_score/differences parsato dal JSON."""
    _patch_routing(monkeypatch, "google", "gemini-x")
    captured: dict[str, Any] = {}
    _patch_gateway(
        monkeypatch,
        ProviderResult(
            provider="google",
            model="gemini-x",
            content='{"similarity_score": 82, "differences": [{"category": "colore", "severity": "media", "description": "x", "suggested_fix": "y"}]}',
            metadata={"stop_reason": "end_turn"},
        ),
        captured,
    )

    body = vision.VisualCompareRequest(
        screenshot_base64=_png_b64(),
        screenshot_mime="image/png",
        reference_base64=_png_b64(),
        reference_mime="image/jpeg",
    )
    out = await vision.vision_compare(body)

    assert out["similarity_score"] == 82
    assert len(out["differences"]) == 1
    assert out["model_used"] == "google/gemini-x"

    # Due immagini nell'ordine screenshot -> reference.
    content = captured["messages"][0]["content"]
    images = [b for b in content if b.get("type") == "image_url"]
    assert len(images) == 2
    assert images[0]["image_url"]["url"].startswith("data:image/png;base64,")
    assert images[1]["image_url"]["url"].startswith("data:image/jpeg;base64,")
