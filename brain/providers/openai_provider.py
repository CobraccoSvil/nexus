"""OpenAI provider implementation."""
from __future__ import annotations

import logging
import os
from typing import Any, AsyncIterator

# Modelli della serie "reasoning" di OpenAI che non accettano temperature, top_p
# e usano max_completion_tokens invece di max_tokens.
#
# Aggiornato 2026-05-20: aggiunti gpt-5 family e gpt-4.5. Prima i log brain
# erano pieni di "Unsupported parameter: 'max_tokens' is not supported with
# this model. Use 'max_completion_tokens' instead." per gpt-5, gpt-5-mini,
# gpt-5.4, gpt-5.5 ecc. che ricadevano nel ramo "use max_tokens".
_O_SERIES_MODELS = frozenset({
    # Reasoning models (originale)
    "o1", "o1-mini", "o1-preview", "o3", "o3-mini", "o4-mini",
    # GPT-5 family (rilasciati 2025+): tutti richiedono max_completion_tokens
    "gpt-5", "gpt-5-mini", "gpt-5-nano",
    "gpt-5.2", "gpt-5.3", "gpt-5.4", "gpt-5.4-mini", "gpt-5.4-nano", "gpt-5.5",
    # GPT-4.5 family (anche reasoning)
    "gpt-4.5", "gpt-4.5-preview",
})

# Modelli OpenAI che NON sono supportati su /v1/chat/completions ma solo
# sull'endpoint /v1/responses (release 2025). Il brain attuale non li
# supporta: ritorniamo errore strutturato cosi' il model_health_probe Rust
# li auto-disabilita dal catalog.
_RESPONSES_ONLY_MODELS = frozenset({
    "gpt-5-pro", "gpt-5-codex", "gpt-5.4-pro", "gpt-5.5-pro", "gpt-5.2-pro",
    "o1-pro", "o3-pro", "o3-deep-research", "o4-mini-deep-research",
})


def _is_responses_only(model: str) -> bool:
    """Modelli OpenAI che richiedono /v1/responses (non supportati dal brain)."""
    model_lower = model.lower()
    return any(model_lower == m or model_lower.startswith(m + "-") for m in _RESPONSES_ONLY_MODELS)

# Soglia: se un modello o-series riceve piu' di N tool, applichiamo il filtro
# safety net lato Python (il filtro principale avviene lato Rust in
# build_tools_json_for_agent, ma il safety net copre il caso di provider
# fallback dove il modello cambia dopo che i tool sono stati costruiti).
_O_SERIES_MAX_TOOLS = 20

# Tool essenziali per modelli o-series (sincronizzati con
# O_SERIES_ESSENTIAL_TOOLS_FALLBACK in brain_agent_client.rs)
_O_SERIES_ESSENTIAL_TOOL_NAMES = frozenset({
    "read_file", "read_file_lines", "list_files", "search_in_files",
    "write_file", "edit_file", "run_command", "fs_mkdir", "delete_file",
    "git_status", "git_commit", "run_tests",
    "nexus_mcp_tool_search", "nexus_mcp_tool_call",
    "search_codebase_semantic",
    # Generazione documenti professionali .docx (audit 27/05/2026):
    # senza questi tool, il pannello DOCUMENTI non riusciva a generare
    # nulla quando il fallback finiva su modelli o-series / gpt-5-nano.
    "nexus_doc_generate", "nexus_doc_update", "nexus_doc_list", "nexus_doc_status",
})


def _filter_essential_tools_o_series(tools: list[dict]) -> list[dict]:
    """Filtra una lista di tool OpenAI-format lasciando solo quelli essenziali.

    Usato come safety net quando il backend Rust non ha potuto filtrare
    (es. dopo provider fallback da Anthropic a OpenAI o-series).
    """
    return [
        t for t in tools
        if t.get("function", {}).get("name", "") in _O_SERIES_ESSENTIAL_TOOL_NAMES
    ]


def _is_o_series(model: str) -> bool:
    """Restituisce True se il modello e' della serie reasoning (o1/o3/o4-mini)."""
    model_lower = model.lower()
    return any(model_lower == m or model_lower.startswith(m + "-") for m in _O_SERIES_MODELS)

from .base import BaseProvider, ProviderCatalogEntry, ProviderResult
from .error_handler import format_error_result

logger = logging.getLogger(__name__)


class OpenAIProvider(BaseProvider):
    name = "openai"

    def __init__(self) -> None:
        # API key letta dal DB (settings.openai_api_key) con cache 60s
        # invece che da env var. Vedi brain/providers/api_key_loader.py.
        from .api_key_loader import load_api_key
        self._api_key_provider = lambda: load_api_key(self.name)
        self._client: Any | None = None
        self._cached_key: str = ""

    @property
    def _api_key(self) -> str:
        # Re-leggi a ogni accesso (cache 60s interna a load_api_key).
        # Se la key cambia (admin update), il client viene reset.
        new_key = self._api_key_provider()
        if new_key != self._cached_key:
            self._cached_key = new_key
            self._client = None  # forza re-init del client AsyncOpenAI con la nuova key
        return new_key

    @_api_key.setter
    def _api_key(self, value: str) -> None:
        # Setter per backward-compat con _load_keys_from_db legacy che fa
        # p._api_key = value. Invalida la cache cosi' la prossima get
        # rilegge la key aggiornata dal DB.
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
            self._client = AsyncOpenAI(api_key=self._api_key, http_client=http_client)
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
                content="[OpenAI API key not configured]",
                metadata={"error": "missing_api_key"},
            )
        # Early return per modelli che richiedono v1/responses (brain non supporta).
        # Il model_health_probe Rust riconosce "model_not_found" e auto-disabilita.
        if _is_responses_only(model):
            return ProviderResult(
                provider=self.name, model=model,
                content=f"[Error: model_not_found — {model} requires v1/responses endpoint, not supported by brain]",
                metadata={"error": "responses_only_model", "model": model},
            )
        try:
            client = self._get_client()
            max_tok = kwargs.get("max_tokens", 4096)
            create_kwargs: dict[str, Any] = {
                "model": model,
                "messages": [{"role": "user", "content": prompt}],
            }
            if _is_o_series(model):
                # I modelli reasoning non accettano temperature/top_p
                # e usano max_completion_tokens al posto di max_tokens.
                create_kwargs["max_completion_tokens"] = max_tok
            else:
                create_kwargs["max_tokens"] = max_tok
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
            logger.error("OpenAI generation failed: %s", e)
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
        """Esegue un turno agente con function calling OpenAI, normalizza output al formato Anthropic."""
        if not self._api_key:
            return ProviderResult(
                provider=self.name, model=model,
                content="[OpenAI API key not configured]",
                metadata={"error": "missing_api_key"},
            )
        if _is_responses_only(model):
            return ProviderResult(
                provider=self.name, model=model,
                content=f"[Error: model_not_found — {model} requires v1/responses endpoint, not supported by brain]",
                metadata={"error": "responses_only_model", "model": model},
            )
        try:
            client = self._get_client()
            # Capability DB-driven (regola G): max_tokens clampato, tool_choice
            # nel dialetto del modello. Degrada ai parametri richiesti se assente.
            cap = None
            try:
                from .capability_loader import load_capability
                from .adapter_base import resolve_max_tokens
                cap = load_capability(self.name, model)
                max_tokens = resolve_max_tokens(cap, max_tokens)
            except Exception as _cap_err:
                logger.warning(
                    "capability %s/%s non disponibile (%s): uso parametri richiesti",
                    self.name, model, _cap_err,
                )
                cap = None
            # Converte il formato Anthropic messages → OpenAI messages
            oai_messages = _convert_messages_to_openai(messages)
            # Inietta system_text come primo messaggio system (cacheable da OpenAI)
            if system_text:
                oai_messages.insert(0, {"role": "system", "content": system_text})
            # Converte Anthropic tool definitions → OpenAI function format
            oai_tools = [_anthropic_tool_to_openai(t) for t in tools] if tools else []

            # Safety net per modelli o-series (vedi anche generate_agent_turn_stream)
            if _is_o_series(model) and len(oai_tools) > _O_SERIES_MAX_TOOLS:
                oai_tools = _filter_essential_tools_o_series(oai_tools)
                logger.info(
                    "o-series safety net (non-stream): tool ridotti a %d per modello %s",
                    len(oai_tools), model,
                )

            kwargs_call: dict[str, Any] = {
                "model": model,
                "messages": oai_messages,
            }
            # I modelli reasoning (o1/o3/o4-mini) usano max_completion_tokens
            # e non accettano temperature, top_p.
            # I system messages devono essere inviati come ruolo "developer"
            # (non "system") per o1/o3 — altrimenti l'API restituisce 400.
            if _is_o_series(model):
                kwargs_call["max_completion_tokens"] = max_tokens
                # Converti il system message già inserito in testa a "developer"
                if oai_messages and oai_messages[0].get("role") == "system":
                    oai_messages[0] = {"role": "developer", "content": oai_messages[0]["content"]}
            else:
                kwargs_call["max_tokens"] = max_tokens
            if oai_tools:
                kwargs_call["tools"] = oai_tools
                # Anti-narration: forza tool_choice=required al primo turno
                # per evitare che il modello narri azioni senza eseguirle.
                # Modelli o-series (reasoning) non accettano tool_choice.
                if not _is_o_series(model):
                    if cap is not None:
                        from .adapter_base import resolve_tool_choice
                        _tc = resolve_tool_choice(cap, oai_messages)
                        if _tc is not None:
                            kwargs_call["tool_choice"] = _tc
                    else:
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

            usage_data = {}
            if response.usage:
                usage_data = {
                    "input_tokens": response.usage.prompt_tokens,
                    "output_tokens": response.usage.completion_tokens,
                }
                # Prompt caching automatico OpenAI: il campo cached_tokens indica
                # quanti token di input sono stati serviti dalla cache (costo 0.5x).
                cached_tokens = 0
                if hasattr(response.usage, "prompt_tokens_details") and response.usage.prompt_tokens_details:
                    cached_tokens = getattr(response.usage.prompt_tokens_details, "cached_tokens", 0) or 0
                if cached_tokens > 0:
                    usage_data["cache_read_input_tokens"] = cached_tokens
                    logger.info(
                        "OpenAI cache hit: %d cached / %d total input tokens (%.0f%%)",
                        cached_tokens,
                        response.usage.prompt_tokens,
                        100.0 * cached_tokens / max(response.usage.prompt_tokens, 1),
                    )

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

    async def generate_agent_turn_stream(
        self,
        model: str,
        messages: list[dict],
        tools: list[dict],
        max_tokens: int = 4096,
        system_text: str = "",
        extended_thinking: bool = False,
    ):
        """Fix M3: streaming agent-turn per OpenAI (paritetico con Anthropic).

        Yield: {"type": "token", "delta": str} per ogni delta di testo
               {"type": "done", "result": dict} al termine (schema generate_agent_turn)
               {"type": "error", "message": str, "metadata"?: dict} in caso di errore

        Nota: il parametro extended_thinking e' ignorato lato OpenAI (reasoning
        traces non sono esposte in stream dall'API chat.completions).
        """
        if not self._api_key:
            yield {"type": "error", "message": "OpenAI API key non configurata"}
            return
        if _is_responses_only(model):
            yield {
                "type": "error",
                "message": f"model_not_found — {model} requires v1/responses endpoint, not supported by brain",
                "metadata": {"error": "responses_only_model", "model": model},
            }
            return
        try:
            import json as _json

            client = self._get_client()
            oai_messages = _convert_messages_to_openai(messages)
            if system_text:
                oai_messages.insert(0, {"role": "system", "content": system_text})
            oai_tools = [_anthropic_tool_to_openai(t) for t in tools] if tools else []

            # Safety net per modelli o-series: se il backend Rust non ha
            # gia' ridotto i tool (es. dopo provider fallback), li filtriamo
            # qui. Iniezione istruzioni esplicite sull'uso dei tool.
            if _is_o_series(model) and len(oai_tools) > _O_SERIES_MAX_TOOLS:
                oai_tools = _filter_essential_tools_o_series(oai_tools)
                logger.info(
                    "o-series safety net: tool ridotti a %d per modello %s",
                    len(oai_tools), model,
                )

            stream_kwargs: dict[str, Any] = {
                "model": model,
                "messages": oai_messages,
                "stream": True,
                "stream_options": {"include_usage": True},
            }
            if _is_o_series(model):
                stream_kwargs["max_completion_tokens"] = max_tokens
                if oai_messages and oai_messages[0].get("role") == "system":
                    oai_messages[0] = {"role": "developer", "content": oai_messages[0]["content"]}
            else:
                stream_kwargs["max_tokens"] = max_tokens
            if oai_tools:
                stream_kwargs["tools"] = oai_tools
                if not _is_o_series(model):
                    from ._schema_utils import resolve_tool_choice_openai
                    stream_kwargs["tool_choice"] = resolve_tool_choice_openai(model, oai_messages)

            text_buf: list[str] = []
            # Accumulatori per tool_calls (delta arrivano frammentati per index)
            tool_calls_acc: dict[int, dict[str, Any]] = {}
            finish_reason: str | None = None
            usage_obj: Any = None

            stream = await client.chat.completions.create(**stream_kwargs)
            async for chunk in stream:
                # Eventuale usage sul chunk finale (stream_options.include_usage)
                if getattr(chunk, "usage", None):
                    usage_obj = chunk.usage
                if not chunk.choices:
                    continue
                choice = chunk.choices[0]
                if getattr(choice, "finish_reason", None):
                    finish_reason = choice.finish_reason
                delta = getattr(choice, "delta", None)
                if delta is None:
                    continue
                # Token di testo
                content_delta = getattr(delta, "content", None)
                if content_delta:
                    text_buf.append(content_delta)
                    yield {"type": "token", "delta": content_delta}
                # Tool calls (delta frammentati)
                tc_deltas = getattr(delta, "tool_calls", None)
                if tc_deltas:
                    for tc_delta in tc_deltas:
                        idx = getattr(tc_delta, "index", 0) or 0
                        slot = tool_calls_acc.setdefault(
                            idx, {"id": None, "name": None, "arguments": ""}
                        )
                        if getattr(tc_delta, "id", None):
                            slot["id"] = tc_delta.id
                        fn = getattr(tc_delta, "function", None)
                        if fn:
                            if getattr(fn, "name", None):
                                slot["name"] = fn.name
                            if getattr(fn, "arguments", None):
                                slot["arguments"] += fn.arguments

            text_content = "".join(text_buf)
            tool_use_blocks: list[dict] = []
            assistant_content: list[dict] = []

            if finish_reason == "tool_calls" and tool_calls_acc:
                stop_reason = "tool_use"
                for _idx in sorted(tool_calls_acc.keys()):
                    slot = tool_calls_acc[_idx]
                    try:
                        args = _json.loads(slot.get("arguments") or "{}")
                    except Exception:
                        args = {}
                    block = {
                        "id": slot.get("id") or f"call_{_idx}",
                        "name": slot.get("name") or "",
                        "input": args,
                    }
                    tool_use_blocks.append(block)
                    assistant_content.append({"type": "tool_use", **block})
            else:
                stop_reason = "end_turn"
                if text_content:
                    assistant_content.append({"type": "text", "text": text_content})

            usage_data: dict[str, Any] = {}
            if usage_obj is not None:
                usage_data = {
                    "input_tokens": getattr(usage_obj, "prompt_tokens", 0) or 0,
                    "output_tokens": getattr(usage_obj, "completion_tokens", 0) or 0,
                }
                cached_tokens = 0
                ptd = getattr(usage_obj, "prompt_tokens_details", None)
                if ptd is not None:
                    cached_tokens = getattr(ptd, "cached_tokens", 0) or 0
                if cached_tokens > 0:
                    usage_data["cache_read_input_tokens"] = cached_tokens

            yield {
                "type": "done",
                "result": {
                    "provider": self.name,
                    "model": model,
                    "content": text_content,
                    "metadata": {
                        "stop_reason": stop_reason,
                        "tool_use_blocks": tool_use_blocks,
                        "assistant_content": assistant_content,
                        "usage": usage_data,
                    },
                },
            }
        except Exception as e:
            meta = format_error_result(e, self.name, model)
            yield {"type": "error", "message": meta.get("error", str(e)), "metadata": meta}

    async def generate_stream(self, model: str, prompt: str, **kwargs: Any) -> AsyncIterator[str]:
        if not self._api_key:
            yield "[OpenAI API key not configured]"
            return
        try:
            client = self._get_client()
            max_tok = kwargs.get("max_tokens", 4096)
            stream_kwargs: dict[str, Any] = {
                "model": model,
                "messages": [{"role": "user", "content": prompt}],
                "stream": True,
            }
            if _is_o_series(model):
                stream_kwargs["max_completion_tokens"] = max_tok
            else:
                stream_kwargs["max_tokens"] = max_tok
                stream_kwargs["temperature"] = kwargs.get("temperature", 0.7)
            stream = await client.chat.completions.create(**stream_kwargs)
            async for chunk in stream:
                delta = chunk.choices[0].delta
                if delta.content:
                    yield delta.content
        except Exception as e:
            logger.error("OpenAI stream failed: %s", e)
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


def _anthropic_tool_to_openai(tool: dict) -> dict:
    """Converte un tool definition Anthropic nel formato OpenAI function.

    Applica anche la compressione dello schema (BP6 piano riduzione token):
    rimuove additionalProperties/$schema/examples, tronca description e enum.
    """
    from ._schema_utils import compress_schema, _truncate_text, DEFAULT_TOOL_DESCR_MAX

    raw_schema = tool.get("input_schema", {"type": "object", "properties": {}})
    return {
        "type": "function",
        "function": {
            "name": tool["name"],
            "description": _truncate_text(tool.get("description", ""), DEFAULT_TOOL_DESCR_MAX),
            "parameters": compress_schema(raw_schema),
        },
    }


def _convert_messages_to_openai(messages: list[dict]) -> list[dict]:
    """Converte messaggi in formato Anthropic (con tool_use/tool_result) in formato OpenAI."""
    import json as _json
    result = []
    for msg in messages:
        role = msg.get("role", "user")
        content = msg.get("content", "")

        if isinstance(content, str):
            result.append({"role": role, "content": content})
        elif isinstance(content, list):
            # Blocchi misti testo/tool_use/tool_result
            text_parts: list[str] = []
            tool_calls: list[dict] = []
            tool_results: list[dict] = []

            for block in content:
                btype = block.get("type")
                if btype == "text":
                    text_parts.append(block.get("text", ""))
                elif btype == "tool_use":
                    tool_calls.append({
                        "id": block["id"],
                        "type": "function",
                        "function": {
                            "name": block["name"],
                            "arguments": _json.dumps(block.get("input", {})),
                        },
                    })
                elif btype == "tool_result":
                    tool_results.append({
                        "role": "tool",
                        "tool_call_id": block["tool_use_id"],
                        "content": block.get("content", ""),
                    })

            if tool_results:
                result.extend(tool_results)
            elif tool_calls:
                oai_msg: dict[str, Any] = {"role": "assistant"}
                if text_parts:
                    oai_msg["content"] = " ".join(text_parts)
                else:
                    oai_msg["content"] = None
                oai_msg["tool_calls"] = tool_calls
                result.append(oai_msg)
            else:
                result.append({"role": role, "content": " ".join(text_parts)})
        else:
            result.append({"role": role, "content": str(content)})

    return result
