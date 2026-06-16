"""Provider locale Ollama — modelli on-premise senza dipendenze cloud.

CHIAMATE LLM: NON eseguite da questo adapter. Dopo il consolidamento del
trasporto (regola L / ADR 0026) tutte le chiamate (generate / agent turn)
passano dal ``GatewayProvider`` (delega al gateway Rust). Questo adapter resta
costruito SOLO per i metodi NON-chiamata: ``list_models`` (catalog UI) e
``test_connection`` (health-check / discovery modelli installati via /api/tags).

Supporta qualsiasi modello installato in Ollama (ollama.ai):
- DeepSeek-R1 distill (privato, nessun dato cloud)
- Qwen 2.5 Coder (coding locale)
- Llama 3.x (universale)

Configurazione:
  OLLAMA_URL=http://localhost:11434   (default)
  OLLAMA_ENABLED=true|false           (default: true se URL raggiungibile)
"""
from __future__ import annotations

import logging
import os
from typing import Any

import httpx

from .base import BaseProvider, ProviderCatalogEntry

logger = logging.getLogger(__name__)

_DEFAULT_OLLAMA_URL = "http://localhost:11434"


class OllamaProvider(BaseProvider):
    """Provider locale tramite Ollama — zero privacy risk, dati mai fuori dalla macchina."""

    name = "ollama"

    def __init__(self) -> None:
        self._base_url = os.getenv("OLLAMA_URL", _DEFAULT_OLLAMA_URL).rstrip("/")
        self._client: httpx.AsyncClient | None = None

    def _get_client(self) -> httpx.AsyncClient:
        if self._client is None:
            self._client = httpx.AsyncClient(
                base_url=self._base_url,
                timeout=httpx.Timeout(120.0, connect=5.0),
            )
        return self._client

    def list_models(self) -> list[ProviderCatalogEntry]:
        """Modelli predefiniti — la lista reale si ottiene via /api/tags a runtime."""
        return [
            ProviderCatalogEntry("deepseek-r1:7b",  ["chat", "reasoning", "local"]),
            ProviderCatalogEntry("deepseek-r1:14b", ["chat", "reasoning", "local"]),
            ProviderCatalogEntry("deepseek-r1:32b", ["chat", "reasoning", "local"]),
            ProviderCatalogEntry("qwen2.5-coder:7b",  ["chat", "coding", "local"]),
            ProviderCatalogEntry("qwen2.5-coder:14b", ["chat", "coding", "local"]),
            ProviderCatalogEntry("qwen2.5-coder:32b", ["chat", "coding", "local"]),
            ProviderCatalogEntry("llama3.2:3b",  ["chat", "local", "fast"]),
            ProviderCatalogEntry("llama3.2:8b",  ["chat", "local"]),
            ProviderCatalogEntry("llama3.1:70b", ["chat", "local", "large"]),
        ]

    async def test_connection(self) -> dict[str, Any]:
        try:
            resp = await self._get_client().get("/api/tags", timeout=3.0)
            resp.raise_for_status()
            data = resp.json()
            models = [m["name"] for m in data.get("models", [])]
            return {
                "provider": self.name,
                "status": "ok",
                "models": models,
                "url": self._base_url,
            }
        except httpx.ConnectError:
            return {"provider": self.name, "status": "offline", "reason": f"Ollama non raggiungibile su {self._base_url}"}
        except Exception as e:
            return {"provider": self.name, "status": "error", "reason": str(e)}
