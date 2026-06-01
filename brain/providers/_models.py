"""Tipi canonici del layer provider (ricostruzione M0/M3 del piano).

Contiene le dataclass condivise tra capability_loader, tool_translator,
tool_validator e gli adapter provider:

- ProviderCapability: specchio runtime della tabella nexus_provider_capabilities
  (mig 0240). Fonte unica dei parametri per-modello. Nessun default di business
  qui: i valori arrivano sempre dal DB (regola G).
- CanonicalTool: definizione tool neutra rispetto al provider.
- CanonicalToolUseBlock: tool-call normalizzato (id, name, input) indipendente
  dal dialetto del provider.
- StopReason: enum canonico (stile Anthropic) verso cui i provider normalizzano.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class StopReason(str, Enum):
    """Motivo di terminazione del turno, normalizzato (stile Anthropic).

    I provider mappano il proprio finish/stop su questi valori in fase di
    normalize(). `UNKNOWN` quando il provider non fornisce un segnale chiaro.
    """
    END_TURN = "end_turn"
    TOOL_USE = "tool_use"
    MAX_TOKENS = "max_tokens"
    STOP_SEQUENCE = "stop_sequence"
    ERROR = "error"
    UNKNOWN = "unknown"


@dataclass(slots=True)
class ProviderCapability:
    """Specchio runtime di una riga di nexus_provider_capabilities (mig 0240).

    Caricata da capability_loader.load_capability(provider, model). Tutti i campi
    provengono dal DB; il loader solleva CapabilityUnavailable se la riga manca
    (niente fallback hardcoded).
    """
    provider: str
    model: str
    tool_use: bool
    vision: bool
    thinking: bool
    max_context_tokens: int
    default_max_output_tokens: int
    max_output_tokens_hard: int
    tool_choice_style: str
    tool_choice_first_turn_force: bool
    schema_strict: bool
    schema_dialect: str
    tool_call_format: str
    max_tools_in_request: int | None
    supports_prompt_cache: bool
    prompt_cache_dialect: str | None
    supports_parallel_tools: bool
    stop_reason_dialect: str
    soft_failure_iter_threshold: int
    soft_failure_content_threshold: int
    history_keep_recent_messages: int
    history_max_old_tool_result_chars: int
    request_timeout_seconds: int
    connect_timeout_seconds: int
    tool_result_max_chars: int
    tool_result_max_bytes: int
    tool_result_max_lines: int

    def clamp_max_output_tokens(self, requested: int) -> int:
        """Limita i max_tokens richiesti entro il tetto hard del modello."""
        if requested <= 0:
            return self.default_max_output_tokens
        return min(requested, self.max_output_tokens_hard)


@dataclass(slots=True)
class CanonicalTool:
    """Definizione tool neutra rispetto al provider.

    `input_schema` e' un JSON Schema (stile Anthropic input_schema). Il
    tool_translator lo converte nel dialetto del provider; il tool_validator lo
    usa per validare gli argomenti del tool-call prima dell'esecuzione.
    """
    name: str
    description: str
    input_schema: dict[str, Any] = field(default_factory=dict)

    @classmethod
    def from_anthropic(cls, tool: dict[str, Any]) -> "CanonicalTool":
        """Costruisce da un tool in formato Anthropic ({name, description,
        input_schema}), che e' il formato in cui mcp-core invia i tool al brain."""
        return cls(
            name=str(tool.get("name", "")),
            description=str(tool.get("description", "")),
            input_schema=dict(tool.get("input_schema") or {}),
        )


@dataclass(slots=True)
class CanonicalToolUseBlock:
    """Tool-call normalizzato, indipendente dal dialetto del provider.

    `id` e' l'identificativo del tool-call (tool_use_id Anthropic, tool_call.id
    OpenAI, sintetizzato per Google/text-fallback). `dialect_meta` conserva
    metadati grezzi del provider per audit.
    """
    id: str
    name: str
    input: dict[str, Any] = field(default_factory=dict)
    dialect_meta: dict[str, Any] = field(default_factory=dict)

    def to_anthropic_block(self) -> dict[str, Any]:
        """Rappresentazione in blocco assistant Anthropic (type=tool_use)."""
        return {"type": "tool_use", "id": self.id, "name": self.name, "input": self.input}
