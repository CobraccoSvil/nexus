"""Client Batch API che delega al gateway LLM Rust (crates/nexus-gateway) via HTTP.

Sostituisce l'accesso diretto agli SDK vendor per i batch ANTHROPIC: invece di
parlare con `anthropic.AsyncAnthropic`, inoltra al gateway Rust che possiede la
logica di submit/poll/results (Message Batches API).

Contratto del gateway (vedi crates/nexus-gateway/src/batch.rs):
  - POST /v1/batch
      body  {provider, requests:[{custom_id, model, messages:[{role,content}],
             max_tokens?, temperature?, tools?}]}
      -> {batch_id, status}             status: "in_progress" | "ended"
  - GET  /v1/batch/{provider}/{batch_id}
      -> {status, request_counts:{processing,succeeded,errored,canceled,expired},
          results:[{custom_id, response?:{content, tool_calls?, usage{...},
                    model_used, provider_used, finish_reason}, error?}]}
      `results` e' valorizzato solo quando status == "ended".

Per ANTHROPIC il gateway e' COMPLETO. Per GOOGLE ritorna 501 (Vertex Batch con
file upload non ancora portato): il flusso Google resta su `google_batch.py`,
non passa di qui.

Questo modulo mantiene un contratto di output IDENTICO a quello che gli endpoint
REST `/batch-analyze/*` esponevano con `AnthropicBatchClient` (regola L: il
chiamante non cambia, cambia solo il trasporto):
  - submit_batch()  -> batch_id (str)
  - poll_status()   -> {"status": "in_progress"|"ended", "request_counts": {...}}
  - get_results()   -> [{"custom_id", "content", "error"}]

Riusa il PUNTO UNICO degli helper gateway (regola L): URL, service token, timeout
e blocco metadata vivono in `gateway_provider` (DB-driven, regola G), nessuna
duplicazione di configurazione.

Regola F: nessun prompt/response in chiaro nei log (solo conteggi e status).
Regola G: nessun modello hardcoded — risolto da nexus_purpose_model
purpose='anthropic_batch' (mig 0102/0136) se il chiamante non lo specifica.
"""
from __future__ import annotations

import logging
from typing import Any

import httpx

from .gateway_provider import (
    _complete_timeout_s,
    _gateway_url,
    _service_token,
)

logger = logging.getLogger(__name__)

# Provider gestito da questo client: solo Anthropic (Google -> google_batch.py).
_PROVIDER = "anthropic"

# max_tokens di default richiesto obbligatoriamente dalla Messages API se il
# chiamante non lo specifica. Non e' un nome di modello (regola G): e' il tetto
# di generazione, allineato al default del gateway (DEFAULT_MAX_TOKENS).
_DEFAULT_MAX_TOKENS = 4096


def _resolve_batch_model() -> str:
    """Risolve il modello batch da nexus_purpose_model (purpose='anthropic_batch').

    Niente fallback hardcoded (regola G): errore esplicito se non configurato o
    mcp-core non raggiungibile.
    """
    try:
        from brain.router.service import _routing_client_singleton
        decision = _routing_client_singleton().purpose_model(purpose="anthropic_batch")
        return decision.model
    except Exception as e:  # noqa: BLE001
        raise RuntimeError(
            "nexus_purpose_model purpose='anthropic_batch' non configurato o mcp-core non raggiungibile. "
            f"Applica migrazione 0102/0136: {e}"
        ) from e


def _headers() -> dict[str, str]:
    """Header Authorization + Content-Type per il gateway (riusa il service token
    del punto unico gateway_provider)."""
    return {
        "Authorization": f"Bearer {_service_token()}",
        "Content-Type": "application/json",
    }


class GatewayBatchClient:
    """Client Batch Anthropic che inoltra al gateway LLM Rust via HTTP (httpx async).

    Non costruisce nessun client SDK vendor: il gateway possiede la chiamata reale
    alle Message Batches di Anthropic. Espone lo stesso contratto del precedente
    `AnthropicBatchClient` cosi' gli endpoint REST `/batch-analyze/*` non cambiano.
    """

    def __init__(self) -> None:
        # Nessuno stato: URL/token/timeout sono letti on-demand dal punto unico
        # gateway_provider (DB-driven, regola G), un cambio config non richiede
        # restart del brain.
        pass

    async def submit_batch(
        self,
        requests: list[dict],  # [{"custom_id": str, "system": str, "prompt": str}]
        model: str | None = None,
        max_tokens: int = _DEFAULT_MAX_TOKENS,
    ) -> str:
        """Invia un batch di richieste al gateway. Ritorna il batch_id.

        Converte il formato interno {custom_id, system, prompt} nel contratto del
        gateway (BatchRequestItem: custom_id + LlmRequest{model, messages, ...}).
        Il `system`, se presente, diventa un messaggio role=system (il gateway lo
        estrae come campo `system` del body Messages Anthropic).
        """
        if model is None:
            model = _resolve_batch_model()

        batch_requests: list[dict[str, Any]] = []
        for req in requests:
            messages: list[dict[str, str]] = []
            system_text = req.get("system")
            if system_text:
                messages.append({"role": "system", "content": system_text})
            messages.append({"role": "user", "content": req["prompt"]})
            batch_requests.append({
                "custom_id": req["custom_id"],
                "model": model,
                "messages": messages,
                "max_tokens": max_tokens,
            })

        payload = {"provider": _PROVIDER, "requests": batch_requests}
        url = f"{_gateway_url()}/v1/batch"
        async with httpx.AsyncClient(timeout=_complete_timeout_s()) as client:
            resp = await client.post(url, json=payload, headers=_headers())
            resp.raise_for_status()
            data = resp.json()

        batch_id = data.get("batch_id")
        if not batch_id:
            raise RuntimeError(f"gateway /v1/batch senza batch_id: status={data.get('status')}")
        logger.info("Batch Anthropic creato via gateway: id=%s, richieste=%d", batch_id, len(batch_requests))
        return batch_id

    async def poll_status(self, batch_id: str) -> dict:
        """Controlla lo stato del batch via gateway.

        Ritorna {"status": "in_progress"|"ended", "request_counts": {...}},
        contratto identico al precedente client diretto.
        """
        data = await self._get_batch(batch_id)
        return {
            "status": data.get("status", "in_progress"),
            "request_counts": data.get("request_counts", {}),
        }

    async def get_results(self, batch_id: str) -> list[dict]:
        """Recupera i risultati del batch via gateway.

        Ritorna lista di {"custom_id", "content", "error"} — il gateway espone
        per ogni item {custom_id, response?:{content,...}, error?}; mappiamo
        `response.content` -> `content` per preservare il contratto storico atteso
        dal consumatore (mcp-core tool_batch_analyze_code).
        """
        data = await self._get_batch(batch_id)
        results: list[dict] = []
        for item in data.get("results", []):
            custom_id = item.get("custom_id", "")
            response = item.get("response")
            error = item.get("error")
            if response is not None:
                results.append({
                    "custom_id": custom_id,
                    "content": response.get("content", "") or "",
                    "error": None,
                })
            else:
                results.append({
                    "custom_id": custom_id,
                    "content": "",
                    "error": str(error) if error else "unknown error",
                })
        return results

    async def _get_batch(self, batch_id: str) -> dict[str, Any]:
        """GET /v1/batch/{provider}/{batch_id}. Solleva su status != 2xx."""
        url = f"{_gateway_url()}/v1/batch/{_PROVIDER}/{batch_id}"
        async with httpx.AsyncClient(timeout=_complete_timeout_s()) as client:
            resp = await client.get(url, headers=_headers())
            resp.raise_for_status()
            return resp.json()
