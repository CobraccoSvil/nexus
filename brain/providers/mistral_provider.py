"""Mistral provider implementation (OpenAI-compatible API)."""
from __future__ import annotations

import logging
import os
from typing import Any

from .base import OpenAICompatProviderBase, ProviderResult
from .openai_provider import _anthropic_tool_to_openai
from ._response_parsers import (
    build_agent_turn_error,
    build_agent_turn_result,
    build_generate_result,
)
from ._schema_utils import compress_tool_list, resolve_tool_choice_openai

logger = logging.getLogger(__name__)

BASE_URL = "https://api.mistral.ai/v1"


class MistralProvider(OpenAICompatProviderBase):
    """Provider Mistral (OpenAI-compatible endpoint).

    Plumbing comune (API key/client, catalogo da DB, guard key mancante,
    test_connection) nel punto unico ``OpenAICompatProviderBase``
    (regola L / ADR 0026, Wave E3).
    """

    name = "mistral"
    base_url = BASE_URL
    api_key_label = "Mistral"

    async def generate(self, model: str, prompt: str, **kwargs: Any) -> ProviderResult:
        if not self._api_key:
            return self._missing_api_key_result(model)
        try:
            client = self._get_client()
            response = await client.chat.completions.create(
                model=model,
                messages=[{"role": "user", "content": prompt}],
                max_tokens=kwargs.get("max_tokens", 4096),
                temperature=kwargs.get("temperature", 0.7),
            )
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
        prompt_cache_key: str = "",
    ) -> ProviderResult:
        """Esegue un turno agente. Mistral Large supporta tool use, gli altri no.

        ``prompt_cache_key`` (gap residuo P1): Mistral NON cacha automaticamente
        il prefix come OpenAI/DeepSeek. La cache si attiva passando un
        identificatore stabile per-run/sessione nel body (doc ufficiale
        https://docs.mistral.ai/studio-api/conversations/advanced/prompt-caching):
        richieste con lo stesso ``prompt_cache_key`` e un prefix compatibile sono
        instradate allo stesso nodo che ha gia' il prefisso in cache (cached al
        10% dell'input). Il prefix stabile e' garantito a monte da P3 (compressione
        a generazioni); qui si fornisce solo l'hint di routing. Il valore arriva da
        ``usage_run_id`` (registry, introspezione difensiva): vuoto sul path gRPC
        one-shot, dove il caching non porterebbe beneficio.
        """
        if not self._api_key:
            return self._missing_api_key_result(model)
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

            # Cache esplicita Mistral (gap residuo P1): instrada la richiesta al
            # nodo che ha il prefix in cache. Passato come extra_body per non
            # dipendere dalla versione del client OpenAI (il param viaggia nel
            # body, dove l'endpoint Mistral lo legge). Regola G: nessun valore
            # hardcoded, l'id arriva dal run corrente.
            if prompt_cache_key:
                kwargs_call["extra_body"] = {"prompt_cache_key": prompt_cache_key}

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
                # Cache hit Mistral: cached prompt tokens dal punto unico (regola L).
                # Popola cache_read_input_tokens cosi' compute_turn_cost li scorpora
                # dall'input e applica il prezzo cache (0.1x, catalog mig 0403).
                from .adapter_base import extract_cached_input_tokens
                cache_read = extract_cached_input_tokens(response.usage)
                if cache_read > 0:
                    usage_data["cache_read_input_tokens"] = cache_read
                    logger.info(
                        "Mistral cache hit: %d cached / %d input tokens (%.0f%%) key=%s",
                        cache_read, response.usage.prompt_tokens,
                        100.0 * cache_read / max(response.usage.prompt_tokens, 1),
                        prompt_cache_key or "-",
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
