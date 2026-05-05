"""Anthropic Messages Batch API — per task non urgenti (documentazione, analisi, ottimizzazione).

Flusso:
  1. submit_batch()   → POST /v1/messages/batches → batch_id
  2. poll_status()    → GET  /v1/messages/batches/{id} → {"processing_status": "in_progress"|"ended"}
  3. get_results()    → GET  /v1/messages/batches/{id}/results → lista risultati JSONL

Latenza attesa: da 30 secondi a qualche minuto (non adatto per risposte interattive).
"""
from __future__ import annotations

import logging
import os
from typing import Any

logger = logging.getLogger(__name__)

# Modello di default per batch — preferire haiku per costo ridotto
BATCH_DEFAULT_MODEL = "claude-haiku-4-5-20251001"


class AnthropicBatchClient:
    def __init__(self) -> None:
        self._api_key = os.getenv("ANTHROPIC_API_KEY", "")
        self._client: Any | None = None

    def _get_client(self) -> Any:
        if self._client is None:
            from anthropic import AsyncAnthropic
            from .dns_transport import get_global_dns_transport
            import httpx
            transport = get_global_dns_transport()
            http_client = httpx.AsyncClient(transport=transport) if transport is not None else None
            self._client = AsyncAnthropic(api_key=self._api_key, http_client=http_client)
        return self._client

    async def submit_batch(
        self,
        requests: list[dict],  # [{"custom_id": str, "system": str, "prompt": str}]
        model: str = BATCH_DEFAULT_MODEL,
        max_tokens: int = 4096,
    ) -> str:
        """Invia un batch di richieste. Ritorna il batch_id."""
        if not self._api_key:
            raise ValueError("ANTHROPIC_API_KEY non configurata")

        client = self._get_client()

        batch_requests = []
        for req in requests:
            messages_body: dict = {
                "model": model,
                "max_tokens": max_tokens,
                "messages": [{"role": "user", "content": req["prompt"]}],
            }
            if req.get("system"):
                messages_body["system"] = req["system"]
            batch_requests.append({
                "custom_id": req["custom_id"],
                "params": messages_body,
            })

        result = await client.messages.batches.create(requests=batch_requests)  # type: ignore[attr-defined]
        logger.info("Batch Anthropic creato: id=%s, richieste=%d", result.id, len(batch_requests))
        return result.id

    async def poll_status(self, batch_id: str) -> dict:
        """Controlla lo stato del batch. Ritorna {"status": "in_progress"|"ended", "counts": {...}}."""
        client = self._get_client()
        batch = await client.messages.batches.retrieve(batch_id)  # type: ignore[attr-defined]
        return {
            "status": batch.processing_status,
            "request_counts": {
                "processing": batch.request_counts.processing,
                "succeeded": batch.request_counts.succeeded,
                "errored": batch.request_counts.errored,
                "canceled": batch.request_counts.canceled,
                "expired": batch.request_counts.expired,
            },
        }

    async def get_results(self, batch_id: str) -> list[dict]:
        """Recupera i risultati completati. Ritorna lista di {"custom_id", "content", "error"}."""
        client = self._get_client()
        results = []
        async for item in await client.messages.batches.results(batch_id):  # type: ignore[attr-defined]
            if item.result.type == "succeeded":
                content = ""
                for block in item.result.message.content:
                    if hasattr(block, "text"):
                        content += block.text
                results.append({"custom_id": item.custom_id, "content": content, "error": None})
            else:
                error = getattr(item.result, "error", None)
                results.append({
                    "custom_id": item.custom_id,
                    "content": "",
                    "error": str(error) if error else "unknown error",
                })
        return results
