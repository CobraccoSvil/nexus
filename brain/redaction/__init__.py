"""Client Python per Microsoft Presidio (analyzer + anonymizer).

Architettura (hybrid profile, vedi `infra/docker/docker-compose.onprem.yml`):

  brain (questo modulo) ──HTTP──> presidio-analyzer:5002    (rileva PII)
                       ──HTTP──> presidio-anonymizer:5001  (oscura PII)

Uso:

  from brain.redaction import redact_text, RedactionResult

  result = await redact_text(
      "Mario Rossi vive a Roma, email mario@example.com",
      language="it",
      entities=["PERSON", "EMAIL_ADDRESS", "LOCATION"],
  )
  print(result.anonymized)  # "<PERSON> vive a <LOCATION>, email <EMAIL_ADDRESS>"
  print(result.entities)    # [{"type": "PERSON", "start": 0, "end": 11}, ...]

CLAUDE.md §G: gli URL Presidio sono letti da env / DB settings.
Niente fallback hardcoded a localhost.
"""

from .client import (
    PresidioClient,
    PresidioUnavailable,
    RedactionResult,
    DetectedEntity,
    redact_text,
    analyze_text,
)

__all__ = [
    "PresidioClient",
    "PresidioUnavailable",
    "RedactionResult",
    "DetectedEntity",
    "redact_text",
    "analyze_text",
]
