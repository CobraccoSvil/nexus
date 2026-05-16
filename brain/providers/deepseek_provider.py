"""DeepSeek provider implementation (OpenAI-compatible API)."""
from __future__ import annotations

import logging
import os
from typing import Any, AsyncIterator

# Modelli DeepSeek "reasoning" che non accettano temperature, top_p
# e non supportano tool calling.
_DEEPSEEK_REASONING_MODELS = frozenset({"deepseek-reasoner"})


def _is_deepseek_reasoning(model: str) -> bool:
    """Restituisce True se il modello e' un modello reasoning DeepSeek (R1/R2)."""
    model_lower = model.lower()
    return model_lower in _DEEPSEEK_REASONING_MODELS or "deepseek-r" in model_lower

from .base import BaseProvider, ProviderCatalogEntry, ProviderResult
from .error_handler import format_error_result
from .openai_provider import _anthropic_tool_to_openai, _convert_messages_to_openai
from ._schema_utils import compress_tool_list

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
            create_kwargs: dict[str, Any] = {
                "model": model,
                "messages": [{"role": "user", "content": prompt}],
                "max_tokens": kwargs.get("max_tokens", 4096),
            }
            # I modelli reasoning (deepseek-reasoner / R1) non accettano temperature.
            if not _is_deepseek_reasoning(model):
                create_kwargs["temperature"] = kwargs.get("temperature", 0.7)
            response = await client.chat.completions.create(**create_kwargs)
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

            # I modelli reasoning (deepseek-reasoner / R1) non supportano tool calling
            # e non accettano temperature.
            supports_tools = not _is_deepseek_reasoning(model)
            compressed = compress_tool_list(tools) if tools and supports_tools else []
            oai_tools = [_anthropic_tool_to_openai(t) for t in compressed] if compressed else []

            kwargs_call: dict[str, Any] = {
                "model": model,
                "max_tokens": max_tokens,
                "messages": oai_messages,
            }
            if oai_tools:
                kwargs_call["tools"] = oai_tools
                # Anti-narration: forza tool_choice=required al primo turno.
                from ._schema_utils import resolve_tool_choice_openai
                kwargs_call["tool_choice"] = resolve_tool_choice_openai(
                    model, oai_messages,
                )

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
                # M64: alcuni modelli (deepseek-chat in particolare) talvolta
                # emettono tool_call come XML inline nel content invece di
                # usare il campo tool_calls nativo. Esempio:
                # <invoke name="run_command"><parameter name="command">ls</parameter></invoke>
                # Se rilevato, parsiamo e convertiamo in tool_use_blocks per
                # evitare che finisca nel display come testo grezzo (DSML leak).
                xml_blocks, cleaned_text = _parse_inline_tool_invocations(text_content)
                if xml_blocks:
                    stop_reason = "tool_use"
                    for blk in xml_blocks:
                        tool_use_blocks.append(blk)
                        assistant_content.append({"type": "tool_use", **blk})
                    if cleaned_text.strip():
                        assistant_content.insert(0, {"type": "text", "text": cleaned_text})
                    text_content = cleaned_text
                elif text_content:
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
            stream_kwargs: dict[str, Any] = {
                "model": model,
                "messages": [{"role": "user", "content": prompt}],
                "max_tokens": kwargs.get("max_tokens", 4096),
                "stream": True,
            }
            if not _is_deepseek_reasoning(model):
                stream_kwargs["temperature"] = kwargs.get("temperature", 0.7)
            stream = await client.chat.completions.create(**stream_kwargs)
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


def _parse_inline_tool_invocations(text: str) -> tuple[list[dict], str]:
    """M64: parser di recupero per tool_call emessi come XML inline.

    Alcuni modelli (deepseek-chat, claude antichi) talvolta producono
    nel content una struttura tipo:

        <invoke name="run_command">
          <parameter name="command">ls -la</parameter>
          <parameter name="timeout_secs">120</parameter>
        </invoke>

    Quando il finish_reason non e' 'tool_calls', il content arriva nel
    display come testo grezzo (problema DSML leak visibile in chat).
    Questo helper estrae le invoke con regex e ritorna:
      - list di {id, name, input} (formato tool_use Anthropic-compat)
      - testo "pulito" senza le invoke (eventuale narrazione)

    Ritorna ([], text_originale) se nessun invoke rilevato.
    """
    import re
    import uuid as _uuid

    if not text or "<invoke" not in text:
        return [], text

    # Regex per match l'intero blocco <invoke name="X">...</invoke>
    invoke_re = re.compile(
        r'<invoke\s+name="(?P<name>[^"]+)"\s*>(?P<body>.*?)</invoke>',
        re.DOTALL,
    )
    param_re = re.compile(
        r'<parameter\s+name="(?P<pname>[^"]+)"(?:\s+string="(?P<is_string>true|false)")?\s*>(?P<value>.*?)</parameter>',
        re.DOTALL,
    )

    blocks: list[dict] = []
    cleaned = text
    for m in invoke_re.finditer(text):
        tool_name = m.group("name").strip()
        body = m.group("body") or ""
        params: dict = {}
        for pm in param_re.finditer(body):
            pname = pm.group("pname").strip()
            raw_value = (pm.group("value") or "").strip()
            is_string_attr = pm.group("is_string")
            # Coercion best-effort: se string="false", prova int/bool/null/float
            if is_string_attr == "false" and raw_value:
                lower = raw_value.lower()
                if lower in ("true", "false"):
                    params[pname] = (lower == "true")
                    continue
                if lower == "null":
                    params[pname] = None
                    continue
                try:
                    params[pname] = int(raw_value)
                    continue
                except ValueError:
                    pass
                try:
                    params[pname] = float(raw_value)
                    continue
                except ValueError:
                    pass
                # fallthrough: tienilo come stringa
            params[pname] = raw_value
        if tool_name:
            blocks.append({
                "id": f"toolu_{_uuid.uuid4().hex[:24]}",
                "name": tool_name,
                "input": params,
            })
        # Rimuovi il blocco dal testo pulito
        cleaned = cleaned.replace(m.group(0), "")

    # Pulisci anche eventuali wrapper <tool_calls>...</tool_calls> esterni
    cleaned = re.sub(r"</?tool_calls\s*/?>", "", cleaned, flags=re.IGNORECASE)
    cleaned = re.sub(r"</?DSML\s*/?>", "", cleaned, flags=re.IGNORECASE)
    # Compatta whitespace ridondante
    cleaned = re.sub(r"\n{3,}", "\n\n", cleaned).strip()

    return blocks, cleaned
