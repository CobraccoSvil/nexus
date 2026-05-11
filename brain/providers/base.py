"""Base types and abstract provider interface for LLM providers."""
from __future__ import annotations

import abc
from dataclasses import dataclass, field
from typing import Any, AsyncIterator


@dataclass(slots=True)
class ProviderCatalogEntry:
    id: str
    capabilities: list[str] = field(default_factory=list)
    enabled: bool = True


@dataclass(slots=True)
class ProviderResult:
    provider: str
    model: str
    content: str
    metadata: dict[str, Any] = field(default_factory=dict)


class BaseProvider(abc.ABC):
    """Abstract base for LLM providers."""

    name: str = "base"

    @abc.abstractmethod
    async def generate(self, model: str, prompt: str, **kwargs: Any) -> ProviderResult:
        ...

    @abc.abstractmethod
    async def generate_stream(self, model: str, prompt: str, **kwargs: Any) -> AsyncIterator[str]:
        ...
        yield  # type: ignore

    @abc.abstractmethod
    async def generate_agent_turn(
        self,
        model: str,
        messages: list[dict],
        tools: list[dict],
        max_tokens: int = 4096,
        system_text: str = "",
    ) -> "ProviderResult":
        """Esegue un turno agente con tool calling. Normalizza l'output al formato Anthropic
        (stop_reason, tool_use_blocks, assistant_content, usage con input_tokens/output_tokens)."""
        ...

    @abc.abstractmethod
    async def test_connection(self) -> dict[str, Any]:
        ...

    @abc.abstractmethod
    def list_models(self) -> list[ProviderCatalogEntry]:
        ...
