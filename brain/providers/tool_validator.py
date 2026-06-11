"""Validazione degli argomenti di un tool-call prima dell'esecuzione (M2).

Quando un provider emette un tool-call, gli argomenti vengono validati contro
l'input_schema canonico del tool PRIMA di inoltrare a mcp-core. Cosi un modello
che inventa un parametro inesistente o sbaglia tipo riceve un feedback
strutturato e specifico ("param X mancante", "atteso integer") e puo correggere
al turno successivo, invece di propagare un errore opaco al backend (sblocca
CR-3: Gemini che inventa attachment_id inesistenti).

Best-effort: se lo schema e malformato o jsonschema non e disponibile, ritorna
ok=True (non bloccare il flusso per un problema di validazione lato schema).
"""
from __future__ import annotations

import logging
from dataclasses import dataclass, field
from typing import Any

from ._models import CanonicalTool

logger = logging.getLogger(__name__)


@dataclass(slots=True)
class ValidationResult:
    ok: bool
    errors: list[str] = field(default_factory=list)
    feedback: str = ""


def _format_feedback(tool_name: str, errors: list[str]) -> str:
    """Messaggio di feedback per il modello, in italiano, conciso e azionabile."""
    head = f"Argomenti non validi per il tool `{tool_name}`:"
    bullets = "\n".join(f"- {e}" for e in errors)
    return f"{head}\n{bullets}\nCorreggi gli argomenti e riprova la chiamata."


def validate_tool_args(tool: CanonicalTool, args: dict[str, Any]) -> ValidationResult:
    """Valida `args` contro `tool.input_schema`. ok=True se valido o se la
    validazione non e applicabile (schema vuoto/malformato, libreria assente)."""
    schema = tool.input_schema
    if not isinstance(schema, dict) or not schema:
        return ValidationResult(ok=True)
    try:
        from jsonschema import Draft202012Validator  # type: ignore[import]
    except Exception:
        logger.debug("jsonschema non disponibile: skip validazione tool args")
        return ValidationResult(ok=True)

    try:
        validator = Draft202012Validator(schema)
    except Exception as e:
        logger.debug("schema tool '%s' non compilabile: %s", tool.name, e)
        return ValidationResult(ok=True)

    raw_errors = sorted(validator.iter_errors(args), key=lambda e: list(e.path))
    if not raw_errors:
        return ValidationResult(ok=True)

    messages: list[str] = []
    for err in raw_errors:
        loc = ".".join(str(p) for p in err.path) or "(root)"
        messages.append(f"`{loc}`: {err.message}")
    return ValidationResult(
        ok=False,
        errors=messages,
        feedback=_format_feedback(tool.name, messages),
    )
