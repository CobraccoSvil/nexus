"""OpenAI provider implementation."""
from __future__ import annotations

import logging
import os
from typing import Any

# NB (ADR 0024 / regola L): _is_o_series qui sotto NON e' una decisione di
# CAPABILITY (tool_use/thinking, che vengono dalla vista 0318 via cap), ma un
# QUIRK DI PROTOCOLLO dell'API OpenAI: questi modelli usano max_completion_tokens
# invece di max_tokens, non accettano temperature/top_p ne' tool_choice, e
# vogliono il ruolo "developer" al posto di "system". Sono dettagli di FORMATO
# della richiesta, non modellati come capability: per questo restano legittimamente
# detection-nome (vedi ADR 0030). Una futura colonna uses_max_completion_tokens
# nella vista potrebbe spostarli nel DB, ma non e' una violazione di regola L.
#
# Modelli della serie "reasoning" di OpenAI che non accettano temperature, top_p
# e usano max_completion_tokens invece di max_tokens.
#
# Solo gli o-series (o1/o3/o4) restano elencati: le famiglie GPT-5 e GPT-4.5
# sono coperte da REGOLA DI FAMIGLIA per prefisso in `_is_o_series` (vedi sotto),
# cosi' ogni nuova release (gpt-5.1, gpt-5.6, gpt-5-mini, ...) e' gestita senza
# doverla aggiungere a mano. Prima la lista esatta non includeva `gpt-5.1` ->
# ricadeva nel ramo `max_tokens` -> "Unsupported parameter: 'max_tokens' ...
# Use 'max_completion_tokens' instead." -> 400 -> il model_health_probe lo
# auto-disabilitava e metteva l'intero provider openai in cooldown (regola G:
# niente nomi-modello hardcoded da manutenere).
_O_SERIES_MODELS = frozenset({
    "o1", "o1-mini", "o1-preview", "o3", "o3-mini", "o4-mini",
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
    """Modelli OpenAI che richiedono /v1/responses (non supportati dal brain).

    Oltre alla lista esplicita, REGOLA DI FAMIGLIA: le varianti `-pro`, `-codex`,
    `-deep-research` della famiglia reasoning (gpt-5* / o1 / o3 / o4) non sono
    chat-compatibili (404 "This is not a chat model" su /v1/chat/completions),
    quindi vanno trattate come responses-only senza elencarle (es. gpt-5.1-codex,
    gpt-5.6-pro)."""
    model_lower = model.lower()
    if any(model_lower == m or model_lower.startswith(m + "-") for m in _RESPONSES_ONLY_MODELS):
        return True
    is_reasoning_family = (
        model_lower.startswith("gpt-5")
        or model_lower.startswith("o1")
        or model_lower.startswith("o3")
        or model_lower.startswith("o4")
    )
    if is_reasoning_family and any(s in model_lower for s in ("-pro", "-codex", "-deep-research")):
        return True
    return False

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
    """True se il modello richiede max_completion_tokens (e non accetta
    temperature/top_p, vuole il ruolo 'developer'): serie reasoning o-series
    PIU' l'intera famiglia GPT-5 e GPT-4.5.

    REGOLA DI FAMIGLIA per prefisso: ogni release gpt-5.x (gpt-5.1, gpt-5.6,
    gpt-5-mini, ...) e gpt-4.5* e' coperta senza elencarla a mano (regola G)."""
    model_lower = model.lower()
    if model_lower.startswith("gpt-5") or model_lower.startswith("gpt-4.5"):
        return True
    return any(model_lower == m or model_lower.startswith(m + "-") for m in _O_SERIES_MODELS)

from .base import OpenAICompatProviderBase, ProviderResult
from .error_handler import format_error_result
from ._response_parsers import (
    build_agent_turn_error,
    build_agent_turn_result,
    build_generate_result,
)

logger = logging.getLogger(__name__)


class OpenAIProvider(OpenAICompatProviderBase):
    """Provider OpenAI ufficiale.

    Plumbing comune (API key/client, catalogo da DB, guard key mancante,
    test_connection) nel punto unico ``OpenAICompatProviderBase``
    (regola L / ADR 0026, Wave E3).
    """

    name = "openai"
    api_key_label = "OpenAI"
    # client_max_retries=0: i retry sono governati a livello applicativo
    # (cascade M60 nel registry), non dall'SDK. Su errori non-retriabili come
    # 402/insufficient_quota (credit_balance_too_low) il client OpenAI
    # ritenterebbe comunque, sprecando latenza durante il cascade mentre
    # openai e' gia' in cooldown billing. Vedi FIX cooldown openai.
    client_max_retries = 0

    async def generate(self, model: str, prompt: str, **kwargs: Any) -> ProviderResult:
        if not self._api_key:
            return self._missing_api_key_result(model)
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
            # JSON mode: output JSON sintatticamente valido (Chat Completions).
            if kwargs.get("json_mode"):
                create_kwargs["response_format"] = {"type": "json_object"}
            response = await client.chat.completions.create(**create_kwargs)
            # Coda comune di generate (punto unico _response_parsers, regola L).
            return build_generate_result(self.name, model, response)
        except Exception as e:
            return build_agent_turn_error(e, self.name, model)

    async def generate_agent_turn(
        self,
        model: str,
        messages: list[dict],
        tools: list[dict],
        max_tokens: int = 4096,
        system_text: str = "",
        force_tool_choice: bool | None = None,
    ) -> ProviderResult:
        """Esegue un turno agente con function calling OpenAI, normalizza output al formato Anthropic."""
        if not self._api_key:
            return self._missing_api_key_result(model)
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

            response = await client.chat.completions.create(**kwargs_call)
            choice = response.choices[0]
            msg = choice.message
            text_content = msg.content or ""
            stop_reason = "end_turn"
            tool_use_blocks: list[dict] = []
            assistant_content: list[dict] = []

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
                # Prompt caching automatico OpenAI: il campo cached_tokens indica
                # quanti token di input sono stati serviti dalla cache (costo 0.5x).
                # Lettura dal punto unico condiviso con mistral (regola L).
                from .adapter_base import extract_cached_input_tokens
                cached_tokens = extract_cached_input_tokens(response.usage)
                if cached_tokens > 0:
                    usage_data["cache_read_input_tokens"] = cached_tokens
                    logger.info(
                        "OpenAI cache hit: %d cached / %d total input tokens (%.0f%%)",
                        cached_tokens,
                        response.usage.prompt_tokens,
                        100.0 * cached_tokens / max(response.usage.prompt_tokens, 1),
                    )

            return build_agent_turn_result(
                provider=self.name,
                model=model,
                text_content=text_content,
                stop_reason=stop_reason,
                tool_use_blocks=tool_use_blocks,
                assistant_content=assistant_content,
                usage_data=usage_data,
            )
        except Exception as e:
            return build_agent_turn_error(e, self.name, model)

    async def generate_agent_turn_stream(
        self,
        model: str,
        messages: list[dict],
        tools: list[dict],
        max_tokens: int = 4096,
        system_text: str = "",
    ):
        """Fix M3: streaming agent-turn per OpenAI (paritetico con Anthropic).

        Yield: {"type": "token", "delta": str} per ogni delta di testo
               {"type": "done", "result": dict} al termine (schema generate_agent_turn)
               {"type": "error", "message": str, "metadata"?: dict} in caso di errore
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
                from .adapter_base import extract_cached_input_tokens
                cached_tokens = extract_cached_input_tokens(usage_obj)
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


# _anthropic_tool_to_openai e _convert_messages_to_openai vivono in adapter_base
# (regola L / ADR 0026, S80: prima la dipendenza era invertita - adapter_base
# layer-basso che importava da openai_provider layer-alto). Re-export per
# retrocompatibilita' dei call site esistenti (deepseek_provider, mistral_provider).
from .adapter_base import (  # noqa: E402,F401
    anthropic_tool_to_openai as _anthropic_tool_to_openai,
    convert_messages_to_openai as _convert_messages_to_openai,
)
