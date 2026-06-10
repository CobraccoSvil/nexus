"""Mistral provider implementation (OpenAI-compatible API)."""
from __future__ import annotations

import logging
import os
from typing import Any, AsyncIterator

from .base import (
    ApiKeyClientMixin,
    BaseProvider,
    ProviderCatalogEntry,
    ProviderResult,
    build_openai_compatible_client,
)
from .error_handler import format_error_result
from .openai_provider import _anthropic_tool_to_openai, _convert_messages_to_openai
from ._schema_utils import compress_tool_list, resolve_tool_choice_openai

logger = logging.getLogger(__name__)

BASE_URL = "https://api.mistral.ai/v1"


class MistralProvider(BaseProvider, ApiKeyClientMixin):
    """Provider Mistral (OpenAI-compatible endpoint).

    La gestione API key + client cacheato vive nel mixin ``ApiKeyClientMixin``
    (punto unico, regola L / ADR 0026, Wave C3).
    """

    name = "mistral"

    def __init__(self) -> None:
        self._init_api_key_cache()

    def _create_client(self, api_key: str) -> Any:
        # Punto unico build_openai_compatible_client (regola L, S70).
        return build_openai_compatible_client(api_key, base_url=BASE_URL)

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
            # Contratto dati B (regola L): error_class + http_status strutturati
            # dall'oggetto SDK reale (niente fallback lessicale a valle).
            meta = format_error_result(e, self.name, model)
            return ProviderResult(
                provider=self.name, model=model,
                content=f"[Error: {meta['error']}]",
                metadata=meta,
            )

    async def generate_agent_turn(
        self,
        model: str,
        messages: list[dict],
        tools: list[dict],
        max_tokens: int = 4096,
        system_text: str = "",
        force_tool_choice: bool | None = None,
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
            # Punto unico prepare_openai_compat_request (regola L, S78).
            from .adapter_base import prepare_openai_compat_request
            cap, oai_messages, max_tokens = prepare_openai_compat_request(
                self.name, model, max_tokens, messages, system_text,
            )

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
            # Capability tool_use dalla fonte UNICA (vista 0318 / ADR 0024) via
            # cap.tool_use: niente piu' decisione di capability dal nome modello
            # (regola L). L'euristica _TOOL_CAPABLE sul nome resta SOLO come
            # fallback se la riga capability manca (cap is None), per non bloccare
            # modelli non ancora presenti in vista (degrado safe, come google_provider).
            if cap is not None:
                supports_tools = cap.tool_use
            else:
                _TOOL_CAPABLE = ("large", "medium", "small", "codestral", "ministral", "nemo", "pixtral")
                supports_tools = any(tag in model.lower() for tag in _TOOL_CAPABLE)
            compressed = compress_tool_list(tools) if tools and supports_tools else []
            oai_tools = [_anthropic_tool_to_openai(t) for t in compressed] if compressed else []

            kwargs_call: dict[str, Any] = {
                "model": model,
                "max_tokens": max_tokens,
                "messages": oai_messages,
            }
            if oai_tools:
                kwargs_call["tools"] = oai_tools
                # Mistral small/ministral/nemo causano loop con "required" → weak_models.
                # Per large/medium/codestral/pixtral: "required" al primo turno,
                # "auto" dopo (helper centralizzato in _schema_utils).
                if cap is not None:
                    from .adapter_base import resolve_tool_choice
                    _tc = resolve_tool_choice(
                        cap, oai_messages,
                        weak_models=("small", "ministral", "nemo"),
                        force_override=force_tool_choice,
                    )
                    if _tc is not None:
                        kwargs_call["tool_choice"] = _tc
                else:
                    kwargs_call["tool_choice"] = resolve_tool_choice_openai(
                        model, oai_messages,
                        weak_models=("small", "ministral", "nemo"),
                        force_override=force_tool_choice,
                    )

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

            # Punto unico in _response_parsers (regola L, S61).
            from ._response_parsers import parse_openai_compatible_choice
            stop_reason, text_content, tool_use_blocks, assistant_content = (
                parse_openai_compatible_choice(msg, choice.finish_reason, tools, text_content)
            )

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
