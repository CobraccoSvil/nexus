"""Registry canonico dei tool agente (M1 del piano).

I tool agente Nexus sono definiti in mcp-core (AGENT_TOOLS_JSON) e inviati al
brain via gRPC come list[dict] in formato Anthropic ({name, description,
input_schema}). Questo modulo li normalizza in CanonicalTool, fonte unica per il
tool_translator (traduzione per dialetto) e il tool_validator (validazione args).

Nota: il piano prevede anche un endpoint REST mcp-core GET /agent/tools/canonical
per caricamento autonomo lato brain; finche non c'e, il registry opera sui tool
gia passati dal chiamante (nessun fallback hardcoded, regola G).
"""
from __future__ import annotations

from typing import Any

from ._models import CanonicalTool


def load_canonical_tools(tools: list[dict[str, Any]] | None) -> list[CanonicalTool]:
    """Converte una lista di tool in formato Anthropic in CanonicalTool.

    Ignora elementi privi di nome. Idempotente e tollerante a input vuoto.
    """
    if not tools:
        return []
    out: list[CanonicalTool] = []
    for t in tools:
        if not isinstance(t, dict):
            continue
        name = t.get("name")
        if not name:
            continue
        out.append(CanonicalTool.from_anthropic(t))
    return out


def build_lookup(tools: list[CanonicalTool]) -> dict[str, CanonicalTool]:
    """Indice nome -> CanonicalTool per validazione veloce dei tool-call."""
    return {t.name: t for t in tools}
