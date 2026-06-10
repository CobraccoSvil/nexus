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

from .base import (
    ApiKeyClientMixin,
    BaseProvider,
    ProviderCatalogEntry,
    ProviderResult,
    build_openai_compatible_client,
)
from .error_handler import format_error_result
from .openai_provider import _anthropic_tool_to_openai, _convert_messages_to_openai
from ._schema_utils import compress_tool_list

logger = logging.getLogger(__name__)

BASE_URL = "https://api.deepseek.com/v1"


class DeepSeekProvider(BaseProvider, ApiKeyClientMixin):
    """Provider DeepSeek (OpenAI-compatible endpoint).

    La gestione API key + client cacheato vive nel mixin
    ``ApiKeyClientMixin`` (punto unico, regola L / ADR 0026, Wave C3).
    """

    name = "deepseek"

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
            # Mig 0390: i task interni TESTUALI (kwarg internal_task=True dai
            # canali gRPC/REST e dai nodi interni del brain) spengono il thinking
            # dei dual-mode V4: senza questo il budget di output finisce in
            # reasoning_content e il content torna vuoto (hollow). Decisione nel
            # PUNTO UNICO should_disable_thinking (regola L); capability
            # best-effort: se manca, nessun cambio di comportamento.
            if kwargs.get("internal_task"):
                from .adapter_base import should_disable_thinking
                try:
                    from .capability_loader import load_capability
                    cap = load_capability(self.name, model)
                except Exception:  # noqa: BLE001
                    cap = None
                if should_disable_thinking(cap, has_tools=False, internal_task=True):
                    create_kwargs["extra_body"] = {"thinking": {"type": "disabled"}}
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
        internal_task: bool = False,
    ) -> ProviderResult:
        """Esegue un turno agente con function calling (deepseek-chat V3+ supporta tool use).

        ``internal_task`` (mig 0390): True per i task interni (purpose, canali
        gRPC/REST di mcp-core) — sui dual-mode V4 spegne il thinking anche nelle
        chiamate SENZA tool, evitando il content vuoto da reasoning overflow.
        """
        if not self._api_key:
            return ProviderResult(
                provider=self.name, model=model,
                content="[DeepSeek API key not configured]",
                metadata={"error": "missing_api_key"},
            )
        try:
            client = self._get_client()
            # Punto unico prepare_openai_compat_request (regola L, S78).
            from .adapter_base import prepare_openai_compat_request
            cap, oai_messages, max_tokens = prepare_openai_compat_request(
                self.name, model, max_tokens, messages, system_text,
            )

            # Capability tool_use dalla fonte UNICA (vista 0318 / ADR 0024) via
            # cap.tool_use: niente piu' decisione di capability dal nome modello
            # (regola L). I modelli reasoning (R1) hanno tool_use=false in vista.
            # L'euristica _is_deepseek_reasoning sul nome resta SOLO come fallback
            # se la riga capability manca (cap is None) (degrado safe, come google_provider).
            if cap is not None:
                supports_tools = cap.tool_use
            else:
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
                if cap is not None:
                    from .adapter_base import resolve_tool_choice
                    _tc = resolve_tool_choice(
                        cap, oai_messages, force_override=force_tool_choice
                    )
                    if _tc is not None:
                        kwargs_call["tool_choice"] = _tc
                else:
                    from ._schema_utils import resolve_tool_choice_openai
                    kwargs_call["tool_choice"] = resolve_tool_choice_openai(
                        model, oai_messages, force_override=force_tool_choice,
                    )

            # ADR 0025 + mig 0390: DeepSeek V4 e' dual-mode e di DEFAULT gira in
            # thinking mode. Il PUNTO UNICO should_disable_thinking (regola L)
            # decide quando forzare il NON-THINKING via il parametro ufficiale
            # extra_body.thinking=disabled (DeepSeek thinking mode guide):
            # - richieste CON tool (ADR 0025): function calling deterministico,
            #   nessun reasoning_content da ri-passare (400 altrimenti);
            # - task interni TESTUALI (internal_task, mig 0390): il reasoning
            #   brucerebbe il budget di output producendo content vuoto (hollow).
            from .adapter_base import should_disable_thinking
            if should_disable_thinking(cap, bool(oai_tools), internal_task):
                kwargs_call["extra_body"] = {"thinking": {"type": "disabled"}}

            response = await client.chat.completions.create(**kwargs_call)
            choice = response.choices[0]
            msg = choice.message
            text_content = msg.content or ""
            # DeepSeek thinking mode: il reasoning_content del turno con tool_calls
            # DEVE essere rispedito nei turni successivi (HTTP 400 altrimenti).
            # Lo conserviamo in assistant_content come blocco type="reasoning";
            # convert_messages_to_openai lo ri-traduce in reasoning_content.
            # Guarded: presente solo con thinking attivo ('native'); con
            # 'disable_for_tools' (default dual-mode) e' vuoto -> no-op.
            reasoning_content = getattr(msg, "reasoning_content", "") or ""
            stop_reason = "end_turn"
            tool_use_blocks: list[dict] = []
            assistant_content: list[dict] = []
            if reasoning_content:
                assistant_content.append(
                    {"type": "reasoning", "reasoning": reasoning_content}
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
                # M64: alcuni modelli (deepseek-chat in particolare) talvolta
                # emettono tool_call come XML inline nel content invece di
                # usare il campo tool_calls nativo. Esempio:
                # <invoke name="run_command"><parameter name="command">ls</parameter></invoke>
                # Se rilevato, parsiamo e convertiamo in tool_use_blocks per
                # evitare che finisca nel display come testo grezzo (DSML leak).
                tool_names = {t.get("name", "") for t in tools if t.get("name")}
                from ._schema_utils import parse_inline_tool_invocations
                xml_blocks, cleaned_text = parse_inline_tool_invocations(text_content, tool_names)
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

            # Diagnostica (regola F: solo lunghezze + finish_reason, niente payload):
            # i modelli deepseek a volte chiudono con content vuoto (la risposta
            # finisce nel reasoning, o il turno e' troncato per length), causando
            # il soft-failure M4 + fallback. Logghiamo i segnali per la causa radice.
            if not text_content.strip() and stop_reason == "end_turn":
                logger.warning(
                    "deepseek %s: content VUOTO a end_turn (finish_reason=%s, "
                    "reasoning_len=%d, completion_tokens=%s) -> soft-failure/fallback probabile",
                    model,
                    choice.finish_reason,
                    len(reasoning_content),
                    response.usage.completion_tokens if response.usage else "?",
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


