"""Test client Presidio: copre il path felice + gestione errori senza
richiedere un'istanza Presidio reale (mock httpx).
"""
from __future__ import annotations

import asyncio
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from brain.redaction import (
    DetectedEntity,
    PresidioClient,
    PresidioUnavailable,
    RedactionResult,
    redact_text,
)


def _mock_httpx_response(status: int = 200, json_body=None):
    resp = MagicMock()
    resp.status_code = status
    resp.is_success = 200 <= status < 300
    resp.json = MagicMock(return_value=json_body or [])
    resp.raise_for_status = MagicMock()
    if status >= 400:
        from httpx import HTTPStatusError
        resp.raise_for_status.side_effect = HTTPStatusError(
            "mock", request=MagicMock(), response=resp,
        )
    return resp


@pytest.mark.asyncio
async def test_detected_entity_roundtrip() -> None:
    """Struttura dataclass: campi e immutabilità."""
    e = DetectedEntity(type="PERSON", start=0, end=11, score=0.95)
    assert e.type == "PERSON"
    assert e.end - e.start == 11
    with pytest.raises(Exception):
        e.start = 100  # type: ignore[misc]


@pytest.mark.asyncio
async def test_redact_text_no_entities_returns_unchanged() -> None:
    """Se analyzer non trova nulla, anonymizer non viene chiamato."""
    with patch("brain.redaction.client.PresidioClient.analyze",
               new=AsyncMock(return_value=[])):
        result: RedactionResult = await redact_text("nessun PII qui", language="en")
    assert result.original == "nessun PII qui"
    assert result.anonymized == "nessun PII qui"
    assert result.entities == []


@pytest.mark.asyncio
async def test_redact_text_calls_anonymizer_when_entities_present() -> None:
    """Path felice: analyzer -> entità -> anonymizer."""
    detected = [DetectedEntity(type="EMAIL_ADDRESS", start=12, end=29, score=0.99)]
    with patch("brain.redaction.client.PresidioClient.analyze",
               new=AsyncMock(return_value=detected)), \
         patch("brain.redaction.client.PresidioClient.anonymize",
               new=AsyncMock(return_value="email: <EMAIL_ADDRESS>")):
        result = await redact_text("email: a@b.com", language="en")
    assert result.anonymized == "email: <EMAIL_ADDRESS>"
    assert result.entities == detected


@pytest.mark.asyncio
async def test_analyze_propagates_presidio_unavailable_on_network_error() -> None:
    """L'analyzer down deve sollevare `PresidioUnavailable` (no magic fallback §G)."""
    client = PresidioClient(analyzer_url="http://invalid-host:9", timeout_s=0.1)
    with pytest.raises(PresidioUnavailable):
        await client.analyze("test")


@pytest.mark.asyncio
async def test_anonymizer_propagates_presidio_unavailable_on_network_error() -> None:
    """Anche l'anonymizer down deve sollevare."""
    client = PresidioClient(anonymizer_url="http://invalid-host:9", timeout_s=0.1)
    with pytest.raises(PresidioUnavailable):
        await client.anonymize("test", [DetectedEntity("X", 0, 4, 0.9)])
