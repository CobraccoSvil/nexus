"""Client httpx async per Presidio (analyzer + anonymizer).

Endpoints letti da env (con default coerenti col compose onprem):
  PRESIDIO_ANALYZER_URL    (default http://presidio-analyzer:5002)
  PRESIDIO_ANONYMIZER_URL  (default http://presidio:5001 / -anonymizer)
  PRESIDIO_TIMEOUT_S       (default 10)

Se Presidio non risponde, solleva `PresidioUnavailable` (no magic fallback
§G — il chiamante decide se bloccare la richiesta o procedere senza redaction).
"""

from __future__ import annotations

import logging
import os
from dataclasses import dataclass
from typing import Any

logger = logging.getLogger(__name__)


class PresidioUnavailable(Exception):
    """Sollevata quando Presidio analyzer/anonymizer non risponde o ritorna 5xx."""


@dataclass(frozen=True)
class DetectedEntity:
    """Entità PII rilevata da Presidio."""
    type: str          # es. "PERSON", "EMAIL_ADDRESS", "PHONE_NUMBER", "LOCATION"
    start: int         # offset inizio nel testo originale
    end: int           # offset fine (esclusivo)
    score: float       # confidence 0..1


@dataclass(frozen=True)
class RedactionResult:
    """Risultato di `redact_text`."""
    original: str
    anonymized: str
    entities: list[DetectedEntity]


def _analyzer_url() -> str:
    return os.environ.get("PRESIDIO_ANALYZER_URL", "http://presidio-analyzer:5002")


def _anonymizer_url() -> str:
    return os.environ.get("PRESIDIO_ANONYMIZER_URL", "http://presidio-anonymizer:5001")


def _timeout() -> float:
    try:
        return float(os.environ.get("PRESIDIO_TIMEOUT_S", "10"))
    except ValueError:
        return 10.0


class PresidioClient:
    """Wrapper httpx async. Tieni un singleton se chiami spesso."""

    def __init__(self,
                 analyzer_url: str | None = None,
                 anonymizer_url: str | None = None,
                 timeout_s: float | None = None) -> None:
        self._analyzer_url = analyzer_url or _analyzer_url()
        self._anonymizer_url = anonymizer_url or _anonymizer_url()
        self._timeout = timeout_s if timeout_s is not None else _timeout()

    async def analyze(
        self,
        text: str,
        *,
        language: str = "en",
        entities: list[str] | None = None,
        score_threshold: float = 0.5,
    ) -> list[DetectedEntity]:
        """Chiama Presidio analyzer e ritorna le entità rilevate.

        Solleva `PresidioUnavailable` se il servizio non risponde.
        """
        # Lazy import per evitare hard dep su httpx in moduli che non usano redaction.
        try:
            import httpx
        except ImportError as exc:
            raise PresidioUnavailable(f"httpx non installato: {exc}") from exc

        payload: dict[str, Any] = {
            "text": text,
            "language": language,
            "score_threshold": score_threshold,
        }
        if entities:
            payload["entities"] = entities

        try:
            async with httpx.AsyncClient(timeout=self._timeout) as client:
                resp = await client.post(f"{self._analyzer_url}/analyze", json=payload)
                resp.raise_for_status()
                rows = resp.json()
        except Exception as exc:
            raise PresidioUnavailable(f"analyzer {self._analyzer_url}: {exc}") from exc

        return [
            DetectedEntity(
                type=str(r.get("entity_type", "UNKNOWN")),
                start=int(r.get("start", 0)),
                end=int(r.get("end", 0)),
                score=float(r.get("score", 0.0)),
            )
            for r in rows
        ]

    async def anonymize(
        self,
        text: str,
        analyzer_results: list[DetectedEntity],
        *,
        replacement: str | None = None,
    ) -> str:
        """Chiama Presidio anonymizer con il risultato dell'analyzer.

        `replacement=None` → operatore "replace" con tag tipo "<PERSON>", "<EMAIL_ADDRESS>".
        Altrimenti sostituisce con la stringa data.
        """
        try:
            import httpx
        except ImportError as exc:
            raise PresidioUnavailable(f"httpx non installato: {exc}") from exc

        operators: dict[str, Any] = {}
        if replacement is not None:
            operators["DEFAULT"] = {"type": "replace", "new_value": replacement}

        payload: dict[str, Any] = {
            "text": text,
            "analyzer_results": [
                {
                    "entity_type": e.type,
                    "start": e.start,
                    "end": e.end,
                    "score": e.score,
                }
                for e in analyzer_results
            ],
        }
        if operators:
            payload["anonymizers"] = operators

        try:
            async with httpx.AsyncClient(timeout=self._timeout) as client:
                resp = await client.post(f"{self._anonymizer_url}/anonymize", json=payload)
                resp.raise_for_status()
                body = resp.json()
        except Exception as exc:
            raise PresidioUnavailable(f"anonymizer {self._anonymizer_url}: {exc}") from exc

        return str(body.get("text", text))

    async def health(self) -> bool:
        """True se entrambi i servizi rispondono al loro /health."""
        try:
            import httpx
        except ImportError:
            return False
        try:
            async with httpx.AsyncClient(timeout=2.0) as client:
                a = await client.get(f"{self._analyzer_url}/health")
                b = await client.get(f"{self._anonymizer_url}/health")
                return a.is_success and b.is_success
        except Exception:
            return False


# ── API top-level convenience ───────────────────────────────────────────────

_default_client: PresidioClient | None = None


def _client() -> PresidioClient:
    global _default_client
    if _default_client is None:
        _default_client = PresidioClient()
    return _default_client


async def analyze_text(
    text: str,
    *,
    language: str = "en",
    entities: list[str] | None = None,
    score_threshold: float = 0.5,
) -> list[DetectedEntity]:
    return await _client().analyze(
        text, language=language, entities=entities, score_threshold=score_threshold
    )


async def redact_text(
    text: str,
    *,
    language: str = "en",
    entities: list[str] | None = None,
    score_threshold: float = 0.5,
    replacement: str | None = None,
) -> RedactionResult:
    """One-shot: analyze + anonymize. Ritorna risultato strutturato."""
    detected = await analyze_text(
        text,
        language=language,
        entities=entities,
        score_threshold=score_threshold,
    )
    if not detected:
        return RedactionResult(original=text, anonymized=text, entities=[])
    anonymized = await _client().anonymize(text, detected, replacement=replacement)
    return RedactionResult(original=text, anonymized=anonymized, entities=list(detected))
