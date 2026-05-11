"""Mistral provider implementation (OpenAI-compatible API)."""
from __future__ import annotations

import logging
import os
from typing import Any, AsyncIterator

from .base import BaseProvider, ProviderCatalogEntry, ProviderResult
from .error_handler import format_error_result
from .openai_provider import _anthropic_tool_to_openai, _convert_messages_to_openai
from ._schema_utils import compress_tool_list

logger = logging.getLogger(__name__)

BASE_URL = "https://api.mistral.ai/v1"


class MistralProvider(BaseProvider):
    name = "mistral"

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
        from .api_key_loader import invalidate_cache
        invalidate_cache(self.name)
        self._cached_key = value or ""
        self._client = None

    def _get_client(self) -> Any:
        if self._client is None:
            from openai import AsyncOpenAI
            from .dns_transport import get_global_dns_transport
            import httpx
            transport = get_global_dns_transport()
            http_client = httpx.AsyncClient(transport=transport) if transport is not None else None
            self._client = AsyncOpenAI(api_key=self._api_key, base_url=BASE_URL, http_client=http_client)
        return self._client

    def list_models(self) -> list[ProviderCatalogEntry]:
        # Lista modelli letta da DB (ai_price_catalog) con cache 60s.
        # Niente fallback hardcoded.
        from .catalog_loader import load_provider_catalog
        return load_provider_catalog(self.name)

    async def generate(self, model: str, prompt: str, **kwargs: Any) -> ProviderResult:
        if not self._api_key:
            return ProviderResult(
                provider=self.name, model=model,
                content="[Mistral API key not configured]",
                metadata={"error": "missing_api_key"},
            )
        try:
            client = self._get_client()
            response = await client.chat.completions.create(
                model=model,
                messages=[{"role": "user", "content": prompt}],
                max_tokens=kwargs.get("max_tokens", 4096),
                temperature=kwargs.get("temperature", 0.7),
            )
            choice = response.choices[0]
            return ProviderResult(
                provider=self.name,
                model=model,
                content=choice.message.content or "",
                metadata={
                    "usage": {
                        "prompt_tokens": response.usage.prompt_tokens,
                        "completion_tokens": response.usage.completion_tokens,
                        "total_tokens": response.usage.total_tokens,
                    },
                    "finish_reason": choice.finish_reason,
                },
            )
        except Exception as e:
            logger.error("Mistral generation failed: %s", e)
            return ProviderResult(
                provider=self.name, model=model,
                content=f"[Error: {e}]",
                metadata={"error": str(e)},
            )

    async def generate_agent_turn(
        self,
        model: str,
        messages: list[dict],
        tools: list[dict],
        max_tokens: int = 4096,
        system_text: str = "",
    ) -> ProviderResult:
        """Esegue un turno agente. Mistral Large supporta tool use, gli altri no."""
        if not self._api_key:
            return ProviderResult(
                provider=self.name, model=model,
                content="[Mistral API key not configured]",
                metadata={"error": "missing_api_key"},
            )
        try:
            client = self._get_client()
            oai_messages = _convert_messages_to_openai(messages)
            if system_text:
                oai_messages.insert(0, {"role": "system", "content": system_text})

            # Modelli Mistral che supportano il function calling / tool use.
            # Riferimento: https://docs.mistral.ai/capabilities/function_calling/
            # (ultimo aggiornamento: 2025-05)
            # - mistral-large-* : sì
            # - mistral-medium-* : sì
            # - mistral-small-* : sì
            # - codestral-*     : sì
            # - ministral-*     : sì
            # - open-mistral-nemo: sì (a partire da mistral-nemo-2407)
            # - open-mistral-7b / mixtral-*: no (modelli deprecati/open weights)
            _TOOL_CAPABLE = ("large", "medium", "small", "codestral", "ministral", "nemo", "pixtral")
            supports_tools = any(cap in model.lower() for cap in _TOOL_CAPABLE)
            compressed = compress_tool_list(tools) if tools and supports_tools else []
            oai_tools = [_anthropic_tool_to_openai(t) for t in compressed] if compressed else []

            kwargs_call: dict[str, Any] = {
                "model": model,
                "max_tokens": max_tokens,
                "messages": oai_messages,
            }
            if oai_tools:
                kwargs_call["tools"] = oai_tools
                # Usa sempre "auto": il modello sceglie liberamente se invocare un tool o
                # rispondere in testo. "any" forzava la tool call ma causava loop infiniti
                # di safety-refusal nei modelli small (il modello generava testo invece di
                # chiamare un tool, riempiendo tutta la finestra di output con ripetizioni).
                kwargs_call["tool_choice"] = "auto"

            response = await client.chat.completions.create(**kwargs_call)
            choice = response.choices[0]
            msg = choice.message
            text_content = msg.content or ""
            stop_reason = "end_turn"
            tool_use_blocks: list[dict] = []
            assistant_content: list[dict] = []

            # Log response details
            logger.warning(
                "Mistral response: model=%s finish_reason=%s tool_calls=%s text_len=%d tools_sent=%d",
                model, choice.finish_reason,
                bool(msg.tool_calls), len(text_content), len(kwargs_call.get("tools", [])),
            )

            # Se finish_reason=length il JSON del tool call è troncato → non tentare il parse
            if choice.finish_reason == "length":
                # Tronca la risposta al primo blocco sensato (<=1500 char) per evitare
                # di restituire all'utente migliaia di ripetizioni da loop di safety-refusal.
                truncated_content = (text_content or "").strip()
                if len(truncated_content) > 1500:
                    # Prende il testo fino al primo punto fermo o fine paragrafo
                    cutoff = truncated_content.find("\n\n", 200)
                    if cutoff == -1:
                        cutoff = truncated_content.find(". ", 200)
                    if cutoff == -1 or cutoff > 1500:
                        cutoff = 1500
                    truncated_content = truncated_content[: cutoff + 1].strip()
                    logger.warning(
                        "Mistral response TRUNCATED (finish_reason=length) — "
                        "testo originale %d char ridotto a %d char (anti-loop).",
                        len(text_content), len(truncated_content),
                    )
                else:
                    logger.error(
                        "Mistral response TRUNCATED (finish_reason=length): "
                        "il modello ha raggiunto max_tokens. tool_calls=%s text_len=%d. "
                        "Restituisco end_turn per evitare parse di JSON incompleto.",
                        bool(msg.tool_calls), len(text_content),
                    )
                return ProviderResult(
                    provider=self.name, model=model,
                    content=truncated_content or "[Risposta troncata: max_tokens raggiunto. Riprova con un messaggio più breve.]",
                    metadata={
                        "stop_reason": "end_turn",
                        "tool_use_blocks": [],
                        "assistant_content": [{"type": "text", "text": text_content}] if text_content else [],
                        "usage": {
                            "input_tokens": response.usage.prompt_tokens if response.usage else 0,
                            "output_tokens": response.usage.completion_tokens if response.usage else 0,
                        },
                    },
                )

            if choice.finish_reason == "tool_calls" and msg.tool_calls:
                stop_reason = "tool_use"
                for tc in msg.tool_calls:
                    import json as _json
                    try:
                        args = _json.loads(tc.function.arguments)
                    except Exception:
                        args = {}
                    block = {"id": tc.id, "name": tc.function.name, "input": args}
                    tool_use_blocks.append(block)
                    assistant_content.append({"type": "tool_use", **block})
            else:
                if text_content:
                    assistant_content.append({"type": "text", "text": text_content})

            usage_data = {}
            if response.usage:
                usage_data = {
                    "input_tokens": response.usage.prompt_tokens,
                    "output_tokens": response.usage.completion_tokens,
                }

            return ProviderResult(
                provider=self.name,
                model=model,
                content=text_content,
                metadata={
                    "stop_reason": stop_reason,
                    "tool_use_blocks": tool_use_blocks,
                    "assistant_content": assistant_content,
                    "usage": usage_data,
                },
            )
        except Exception as e:
            meta = format_error_result(e, self.name, model)
            return ProviderResult(
                provider=self.name, model=model,
                content=f"[Error: {meta['error']}]",
                metadata=meta,
            )

    async def generate_stream(self, model: str, prompt: str, **kwargs: Any) -> AsyncIterator[str]:
        if not self._api_key:
            yield "[Mistral API key not configured]"
            return
        try:
            client = self._get_client()
            stream = await client.chat.completions.create(
                model=model,
                messages=[{"role": "user", "content": prompt}],
                max_tokens=kwargs.get("max_tokens", 4096),
                temperature=kwargs.get("temperature", 0.7),
                stream=True,
            )
            async for chunk in stream:
                delta = chunk.choices[0].delta
                if delta.content:
                    yield delta.content
        except Exception as e:
            logger.error("Mistral stream failed: %s", e)
            yield f"[Error: {e}]"

    async def test_connection(self) -> dict[str, Any]:
        if not self._api_key:
            return {"provider": self.name, "status": "not_configured", "reason": "API key non configurata"}
        try:
            client = self._get_client()
            await client.models.list()
            return {"provider": self.name, "status": "ready"}
        except Exception as e:
            from .error_handler import classify_error
            info = classify_error(e, self.name)
            return {"provider": self.name, "status": "error", "reason": info["message"], "error_class": info["stop_reason"]}
