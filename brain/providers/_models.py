"""Tipi canonici del layer provider (ricostruzione M0/M3 del piano).

Contiene le dataclass condivise tra capability_loader, tool_validator e gli
adapter provider (tool_translator e' stato rimosso: layer dialetti mai
cablato, bonifica dead code 2026-06-11):

- ProviderCapability: specchio runtime della tabella nexus_provider_capabilities
  (mig 0240). Fonte unica dei parametri per-modello. Nessun default di business
  qui: i valori arrivano sempre dal DB (regola G).
- CanonicalTool: definizione tool neutra rispetto al provider.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


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
    # ADR 0025: policy d'uso nei run agentici (dal catalog via v_model_capabilities).
    #   none | disable_for_tools | native | exclude
    # 'disable_for_tools' -> l'adapter forza il NON-THINKING quando ci sono tool.
    agentic_thinking_policy: str = "none"

    def clamp_max_output_tokens(self, requested: int) -> int:
        """Limita i max_tokens richiesti entro il tetto hard del modello."""
        if requested <= 0:
            return self.default_max_output_tokens
        return min(requested, self.max_output_tokens_hard)

    @property
    def thinking_disabled_for_tools(self) -> bool:
        """True se, in una richiesta agentica CON tool, il thinking va spento.

        Semantica della policy 'disable_for_tools' (ADR 0025): i modelli
        dual-mode (claude-4.x, gemini-2.5/3.x, gpt-5.x, deepseek-v4) girano in
        NON-THINKING quando ci sono tool, cosi' il function calling e'
        deterministico e non resta reasoning_content da ri-passare.

        Punto unico (regola L): e' l'unico posto che traduce la policy in
        "thinking off per i tool". Gli adapter provider la usano per decidere se
        inviare i parametri thinking; `thinking_active_with_tools` la usa per il
        gate del tool_choice. Niente confronto-stringa duplicato nei call site.
        """
        return self.agentic_thinking_policy == "disable_for_tools"

    @property
    def thinking_active_with_tools(self) -> bool:
        """True se il modello sta EFFETTIVAMENTE ragionando in una richiesta con tool.

        Un modello thinking-capable (`self.thinking`, da uses_thinking_mode) NON
        e' in thinking mode quando la policy lo disabilita per i tool. Usata da
        `resolve_tool_choice`: con thinking ATTIVO l'API rifiuta un tool_choice
        forzato (-> degrada ad auto); con thinking SPENTO si puo' forzare l'azione
        al primo turno (anti-narration). Prima il gate guardava `self.thinking`
        statico e annullava la forzatura per tutti i dual-mode 'disable_for_tools'
        anche quando il thinking era spento -> "pianifica e non agisce".
        """
        return self.thinking and not self.thinking_disabled_for_tools


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
