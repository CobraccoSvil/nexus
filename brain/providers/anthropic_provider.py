"""Anthropic provider adapter.

CHIAMATE LLM: NON eseguite da questo adapter. Dopo il consolidamento del
trasporto (regola L / ADR 0026) tutte le chiamate (generate / agent turn /
stream) passano dal ``GatewayProvider`` (delega al gateway Rust). Questo adapter
resta costruito SOLO per i metodi NON-chiamata ancora non coperti dal gateway:
``list_models`` (catalog-sync) e ``test_connection`` (health-check admin), piu'
il client SDK on-demand che essi usano. Le quirk Anthropic delle chiamate
(system cache_control, breakpoint cache, extended thinking, compressione
tool_result) vivono ora nel gateway Rust (crates/nexus-gateway/src/providers/anthropic.rs).
"""
from __future__ import annotations

import logging
from typing import Any

from .base import BaseProvider, ProviderCatalogEntry

logger = logging.getLogger(__name__)


def _resolve_test_connection_model() -> str:
    """Risolve il modello per il ping di test_connection() da nexus_purpose_model
    (purpose='provider_test_connection.anthropic', mig 0171). CLAUDE.md §G.
    """
    try:
        from brain.router.service import _routing_client_singleton
        decision = _routing_client_singleton().purpose_model(
            purpose="provider_test_connection.anthropic"
        )
        return decision.model
    except Exception as exc:
        raise RuntimeError(
            "nexus_purpose_model purpose='provider_test_connection.anthropic' "
            f"non configurato: {exc}"
        ) from exc


class AnthropicProvider(BaseProvider):
    name = "anthropic"

    def __init__(self) -> None:
        # API key letta dal DB con cache 60s. Vedi api_key_loader.py.
        from .api_key_loader import load_api_key
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
        # Backward compat con _load_keys_from_db legacy: invalida cache.
        from .api_key_loader import invalidate_cache
        invalidate_cache(self.name)
        self._cached_key = value or ""
        self._client = None

    def _get_client(self) -> Any:
        if self._client is None:
            from anthropic import AsyncAnthropic
            from .dns_transport import get_global_dns_transport
            import httpx
            transport = get_global_dns_transport()
            http_client = httpx.AsyncClient(transport=transport) if transport is not None else None
            self._client = AsyncAnthropic(api_key=self._api_key, http_client=http_client)
        return self._client

    def list_models(self) -> list[ProviderCatalogEntry]:
        # Lista modelli letta da DB (ai_price_catalog) con cache 60s.
        # Solleva ProviderCatalogUnavailable se DB down o tabella vuota:
        # niente fallback hardcoded.
        from .catalog_loader import load_provider_catalog
        return load_provider_catalog(self.name)

    async def test_connection(self) -> dict[str, Any]:
        if not self._api_key:
            return {"provider": self.name, "status": "not_configured", "reason": "API key non configurata"}
        try:
            client = self._get_client()
            test_model = _resolve_test_connection_model()
            await client.messages.create(
                model=test_model,
                max_tokens=10,
                messages=[{"role": "user", "content": "ping"}],
            )
            return {"provider": self.name, "status": "ready"}
        except Exception as e:
            from .error_handler import classify_error
            info = classify_error(e, self.name)
            return {"provider": self.name, "status": "error", "reason": info["message"], "error_class": info["stop_reason"]}
