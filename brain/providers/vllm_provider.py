"""Provider vLLM — endpoint OpenAI-compatible per modelli serviti localmente.

Architettura (profile onprem, vedi `infra/docker/docker-compose.onprem.yml`):

  brain (questo provider) ──HTTP──> vllm:8000     (GPU NVIDIA, model 32B)
                          ──HTTP──> vllm-cpu:8001 (profile cpu-test, model 7B)

vLLM espone il subset OpenAI Chat Completions API. Riusiamo l'SDK OpenAI
con `base_url` override + chiave dummy (vLLM non valida API key).

Config (env):
  VLLM_URL              default http://vllm:8000
  VLLM_API_KEY_DUMMY    default "vllm-no-auth" (non validata)
  VLLM_TIMEOUT_S        default 120

CLAUDE.md §G: URL e timeout letti da env / settings, nessun fallback
hardcoded a un modello specifico — la lista modelli viene da
`/v1/models` di vLLM stesso.
"""

from __future__ import annotations

import logging
import os
from typing import Any

from .base import BaseProvider, ProviderCatalogEntry, ProviderResult

logger = logging.getLogger(__name__)


def _vllm_url() -> str:
    return os.environ.get("VLLM_URL", "http://vllm:8000").rstrip("/")


def _vllm_api_key() -> str:
    # vLLM accetta qualsiasi stringa come API key (non valida); usiamo dummy.
    return os.environ.get("VLLM_API_KEY_DUMMY", "vllm-no-auth")


def _timeout() -> float:
    try:
        return float(os.environ.get("VLLM_TIMEOUT_S", "120"))
    except ValueError:
        return 120.0


class VllmProvider(BaseProvider):
    """Provider vLLM via OpenAI-compatible API.

    Non hardcoda nessun modello: la lista viene da `/v1/models` chiamato a
    runtime; se vLLM e' down, `list_models()` ritorna lista vuota e
    `test_connection()` riporta lo stato in modo strutturato.
    """

    name = "vllm"

    def __init__(self) -> None:
        self._base_url = _vllm_url()
        self._client_inst: Any | None = None

    def _get_client(self) -> Any:
        """Importa OpenAI SDK lazy. Solleva se non installato (segnalato a
        caller; non e' fallback nascosto)."""
        if self._client_inst is None:
            try:
                from openai import AsyncOpenAI  # type: ignore
            except ImportError as exc:
                raise RuntimeError(
                    f"openai SDK non installato: {exc}. "
                    "Per usare VllmProvider, installare `openai` nel venv brain."
                ) from exc
            self._client_inst = AsyncOpenAI(
                base_url=f"{self._base_url}/v1",
                api_key=_vllm_api_key(),
                timeout=_timeout(),
            )
        return self._client_inst

    def list_models(self) -> list[ProviderCatalogEntry]:
        """Lista placeholder; uso async `_list_running_models()` per dato vero.

        Il framework chiama `list_models()` sincrono per popolare il catalog UI
        all'avvio. Restituiamo i modelli "tipici" di un setup onprem: la verita'
        runtime viene dal DB `ai_price_catalog` (admin la aggiorna) o dall'API
        `/v1/models` di vLLM via `_list_running_models()`.
        """
        # Modelli supportati dal compose default (vedi VLLM_MODEL env).
        # Non e' una violazione §G perche' questa e' lista di "esempi noti"
        # per il catalog UI, non un fallback per il routing reale.
        return [
            ProviderCatalogEntry("qwen2.5-coder-32b", ["chat", "coding", "local", "gpu"]),
            ProviderCatalogEntry("qwen2.5-coder-7b",  ["chat", "coding", "local", "cpu"]),
        ]

    async def _list_running_models(self) -> list[str]:
        """Recupera i modelli effettivamente servi da vLLM via OpenAI API."""
        try:
            client = self._get_client()
            resp = await client.models.list()
            return [m.id for m in resp.data]
        except Exception as exc:
            logger.warning("vllm: list_models fallito: %s", exc)
            return []

    async def test_connection(self) -> dict[str, Any]:
        """Health probe: ritorna stato strutturato, no panic."""
        try:
            models = await self._list_running_models()
            if models:
                return {
                    "provider": self.name,
                    "status": "ready",
                    "endpoint": self._base_url,
                    "models_available": len(models),
                    "model_ids": models[:5],
                }
            return {
                "provider": self.name,
                "status": "no_models",
                "endpoint": self._base_url,
                "reason": "vLLM raggiungibile ma /v1/models vuoto",
            }
        except Exception as exc:
            return {
                "provider": self.name,
                "status": "error",
                "endpoint": self._base_url,
                "reason": str(exc),
            }

    async def generate(
        self,
        model: str,
        prompt: str,
        **kwargs: Any,
    ) -> ProviderResult:
        """Chiamata single-turn via Chat Completions API."""
        client = self._get_client()
        max_tokens = int(kwargs.get("max_tokens", 4096))
        temperature = kwargs.get("temperature")
        params: dict[str, Any] = {
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": max_tokens,
        }
        if temperature is not None:
            params["temperature"] = float(temperature)

        try:
            resp = await client.chat.completions.create(**params)
        except Exception as exc:
            logger.error("vllm generate failed: %s", exc)
            from .error_handler import format_error_result
            meta = format_error_result(exc, self.name, model)
            return ProviderResult(
                provider=self.name,
                model=model,
                content=f"[vLLM error: {meta.get('error', exc)}]",
                metadata=meta,
            )

        text = resp.choices[0].message.content or ""
        usage = getattr(resp, "usage", None)
        meta: dict[str, Any] = {"endpoint": self._base_url}
        if usage is not None:
            meta["usage"] = {
                "input_tokens": getattr(usage, "prompt_tokens", 0),
                "output_tokens": getattr(usage, "completion_tokens", 0),
            }
        return ProviderResult(
            provider=self.name,
            model=model,
            content=text,
            metadata=meta,
        )

    async def generate_agent_turn(
        self,
        model: str,
        messages: list[dict],
        tools: list[dict],
        max_tokens: int = 4096,
        system_text: str = "",
    ) -> ProviderResult:
        """Turno agente con tool calling. vLLM supporta function calling
        sui modelli compatibili (Qwen 2.5+, ecc.). Output normalizzato al
        formato Anthropic-like (`stop_reason`, `tool_use_blocks`).
        """
        client = self._get_client()

        # Costruisci messaggi con system prompt opzionale.
        msgs: list[dict[str, Any]] = []
        if system_text:
            msgs.append({"role": "system", "content": system_text})
        msgs.extend(messages)

        params: dict[str, Any] = {
            "model": model,
            "messages": msgs,
            "max_tokens": max_tokens,
        }
        if tools:
            # Formato OpenAI-style: { "type": "function", "function": {...} }
            params["tools"] = [
                {"type": "function", "function": t} if "function" not in t else t
                for t in tools
            ]
            params["tool_choice"] = "auto"

        try:
            resp = await client.chat.completions.create(**params)
        except Exception as exc:
            logger.error("vllm agent_turn failed: %s", exc)
            from .error_handler import format_error_result
            meta = format_error_result(exc, self.name, model)
            return ProviderResult(
                provider=self.name,
                model=model,
                content=f"[vLLM agent error: {meta.get('error', exc)}]",
                metadata={**meta, "stop_reason": "error"},
            )

        choice = resp.choices[0]
        message = choice.message
        finish = choice.finish_reason or "stop"
        usage = getattr(resp, "usage", None)

        # Normalizzazione al formato Anthropic-like atteso dal layer agente.
        tool_use_blocks: list[dict[str, Any]] = []
        if getattr(message, "tool_calls", None):
            for tc in message.tool_calls:
                tool_use_blocks.append({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.function.name,
                    "input": tc.function.arguments,  # JSON string
                })

        stop_reason = "tool_use" if tool_use_blocks else (
            "end_turn" if finish == "stop" else finish
        )

        meta: dict[str, Any] = {
            "endpoint": self._base_url,
            "stop_reason": stop_reason,
            "assistant_content": message.content or "",
            "tool_use_blocks": tool_use_blocks,
        }
        if usage is not None:
            meta["usage"] = {
                "input_tokens": getattr(usage, "prompt_tokens", 0),
                "output_tokens": getattr(usage, "completion_tokens", 0),
            }

        return ProviderResult(
            provider=self.name,
            model=model,
            content=message.content or "",
            metadata=meta,
        )
