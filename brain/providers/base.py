"""Base types and abstract provider interface for LLM providers."""
from __future__ import annotations

import abc
from dataclasses import dataclass, field
from typing import Any


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


class ApiKeyClientMixin:
    """Mixin: gestione comune della API key (lettura DB + cache 60s) e del client
    cacheato (invalidato se la key cambia). Punto unico (regola L / ADR 0026,
    Wave C3): prima questo blocco era duplicato pari-pari in
    deepseek_provider.py, mistral_provider.py, openai_provider.py (cluster top
    del jscpd report: 43L cross-provider).

    Le sottoclassi devono:
      - settare ``self.name`` (gia' previsto da ``BaseProvider``);
      - implementare ``_create_client(self, api_key: str) -> Any`` che
        costruisce il client SDK reale (es. ``AsyncOpenAI(api_key=...,
        base_url=...)``). Il mixin si occupa di invalidare/ricreare quando
        la key cambia.

    Uso::

        class FooProvider(BaseProvider, ApiKeyClientMixin):
            name = "foo"
            def __init__(self) -> None:
                self._init_api_key_cache()
            def _create_client(self, api_key: str) -> Any:
                return FooClient(api_key=api_key)
    """

    name: str  # fornito da BaseProvider

    def _init_api_key_cache(self) -> None:
        """Inizializza i campi del mixin. Chiamare dal ``__init__`` della sottoclasse."""
        from .api_key_loader import load_api_key  # import locale per evitare cicli

        self._api_key_provider = lambda: load_api_key(self.name)
        self._client: Any | None = None
        self._cached_key: str = ""

    @property
    def _api_key(self) -> str:
        new_key = self._api_key_provider()
        if new_key != self._cached_key:
            self._cached_key = new_key
            self._client = None
        return new_key

    @_api_key.setter
    def _api_key(self, value: str) -> None:
        from .api_key_loader import invalidate_cache

        invalidate_cache(self.name)
        self._cached_key = value or ""
        self._client = None

    def _get_client(self) -> Any:
        """Restituisce il client cacheato (lo crea on-demand alla prima chiamata
        o quando la API key cambia). Chiama ``_create_client`` della sottoclasse."""
        if self._client is None:
            self._client = self._create_client(self._api_key)  # type: ignore[attr-defined]
        return self._client

    def _create_client(self, api_key: str) -> Any:
        """Hook da implementare nella sottoclasse: costruisce il client SDK."""
        raise NotImplementedError(
            f"{type(self).__name__} deve implementare _create_client(api_key)"
        )


def build_openai_compatible_client(
    api_key: str,
    *,
    base_url: str | None = None,
    max_retries: int | None = None,
) -> Any:
    """Costruisce un ``openai.AsyncOpenAI`` con il transport DNS condiviso
    Nexus (httpx). Punto unico (regola L / ADR 0026, S70) per il pattern
    `import + transport + AsyncOpenAI(...)` prima duplicato in
    `deepseek_provider`, `mistral_provider`, `openai_provider`.

    - ``base_url=None`` per il provider OpenAI ufficiale, stringa per i
      compatibili (deepseek, mistral, groq, ecc.).
    - ``max_retries=Some(0)`` per gli adapter che vogliono cascade applicativa
      (es. openai cooldown billing); ``None`` per il default SDK.
    """
    from openai import AsyncOpenAI  # type: ignore[import]
    import httpx  # type: ignore[import]

    from .dns_transport import get_global_dns_transport

    transport = get_global_dns_transport()
    http_client = (
        httpx.AsyncClient(transport=transport) if transport is not None else None
    )
    kwargs: dict[str, Any] = {"api_key": api_key, "http_client": http_client}
    if base_url is not None:
        kwargs["base_url"] = base_url
    if max_retries is not None:
        kwargs["max_retries"] = max_retries
    return AsyncOpenAI(**kwargs)
