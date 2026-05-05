"""DeepSeek provider implementation (OpenAI-compatible API)."""
from __future__ import annotations

import logging
import os
from typing import Any, AsyncIterator

from .base import BaseProvider, ProviderCatalogEntry, ProviderResult
from .error_handler import format_error_result
from .openai_provider import _anthropic_tool_to_openai, _convert_messages_to_openai

logger = logging.getLogger(__name__)

BASE_URL = "https://api.deepseek.com/v1"


class DeepSeekProvider(BaseProvider):
    name = "deepseek"

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
                content="[DeepSeek API key not configured]",
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
            logger.error("DeepSeek generation failed: %s", e)
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
        """Esegue un turno agente con function calling (deepseek-chat V3+ supporta tool use)."""
        if not self._api_key:
            return ProviderResult(
                provider=self.name, model=model,
                content="[DeepSeek API key not configured]",
                metadata={"error": "missing_api_key"},
            )
        try:
            client = self._get_client()
            oai_messages = _convert_messages_to_openai(messages)
            if system_text:
                oai_messages.insert(0, {"role": "system", "content": system_text})

            # deepseek-reasoner non supporta tool calling
            supports_tools = model != "deepseek-reasoner"
            oai_tools = [_anthropic_tool_to_openai(t) for t in tools] if tools and supports_tools else []

            kwargs_call: dict[str, Any] = {
                "model": model,
                "max_tokens": max_tokens,
                "messages": oai_messages,
            }
            if oai_tools:
                kwargs_call["tools"] = oai_tools

            response = await client.chat.completions.create(**kwargs_call)
            choice = response.choices[0]
            msg = choice.message
            text_content = msg.content or ""
            stop_reason = "end_turn"
            tool_use_blocks: list[dict] = []
            assistant_content: list[dict] = []

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
            yield "[DeepSeek API key not configured]"
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
            logger.error("DeepSeek stream failed: %s", e)
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
