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
    async def test_connection(self) -> dict[str, Any]:
        ...

    @abc.abstractmethod
    def list_models(self) -> list[ProviderCatalogEntry]:
        ...
