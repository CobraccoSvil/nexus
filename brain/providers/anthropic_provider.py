"""Anthropic provider implementation."""
from __future__ import annotations

import logging
import os
from typing import Any, AsyncIterator

from .base import BaseProvider, ProviderCatalogEntry, ProviderResult
from .error_handler import format_error_result
from ._schema_utils import compress_tool_list, measure_tools_bytes
from brain.agents import thinking_config as _thinking_config

logger = logging.getLogger(__name__)


# TTL cache per il blocco system prompt (BP2 piano riduzione token).
# Anthropic supporta "5m" (default) e "1h" (beta extended-cache-ttl).
# Il system prompt cambia raramente: 1h massimizza il cache hit rate fra turni
# distanti. I breakpoint sulla history restano sul default 5m perche' la storia
# muta ad ogni turno.
# Valore canonico: settings.anthropic_system_cache_ttl nel DB (admin panel).
# Override emergenza: NEXUS_ANTHROPIC_SYSTEM_CACHE_TTL=5m (priorita' massima).
def _load_system_cache_ttl() -> str:
    from brain.utils.settings_db import get_setting as _gs
    # 1. Env var override (emergenza)
    env_val = os.getenv("NEXUS_ANTHROPIC_SYSTEM_CACHE_TTL", "").strip()
    if env_val in ("5m", "1h"):
        return env_val
    if env_val:
        logger.warning("NEXUS_ANTHROPIC_SYSTEM_CACHE_TTL=%r non valido, ignoro e leggo DB", env_val)
    # 2. DB
    db_val = _gs("anthropic_system_cache_ttl", "1h").strip()
    if db_val not in ("5m", "1h"):
        logger.warning("anthropic_system_cache_ttl DB=%r non valido, uso '1h'", db_val)
        return "1h"
    return db_val

_SYSTEM_CACHE_TTL = _load_system_cache_ttl()


def _system_cache_control() -> dict:
    """Cache control block per il system prompt.

    Per TTL 1h serve il beta header 'extended-cache-ttl-2025-04-11' che il
    client Anthropic aggiunge automaticamente quando rileva il campo ttl.
    """
    if _SYSTEM_CACHE_TTL == "5m":
        return {"type": "ephemeral"}
    return {"type": "ephemeral", "ttl": _SYSTEM_CACHE_TTL}


class AnthropicProvider(BaseProvider):
    name = "anthropic"

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
        # Backward compat con _load_keys_from_db legacy: invalida cache.
        from .api_key_loader import invalidate_cache
        invalidate_cache(self.name)
        self._cached_key = value or ""
        self._client = None

    def _get_client(self) -> Any:
        if self._client is None:
            from anthropic import AsyncAnthropic
            from .dns_transport import get_global_dns_transport
            import httpx
            transport = get_global_dns_transport()
            http_client = httpx.AsyncClient(transport=transport) if transport is not None else None
            self._client = AsyncAnthropic(api_key=self._api_key, http_client=http_client)
        return self._client

    def list_models(self) -> list[ProviderCatalogEntry]:
        # Lista modelli letta da DB (ai_price_catalog) con cache 60s.
        # Solleva ProviderCatalogUnavailable se DB down o tabella vuota:
        # niente fallback hardcoded.
        from .catalog_loader import load_provider_catalog
        return load_provider_catalog(self.name)

    async def generate(self, model: str, prompt: str, **kwargs: Any) -> ProviderResult:
        if not self._api_key:
            return ProviderResult(
                provider=self.name, model=model,
                content="[Anthropic API key not configured]",
                metadata={"error": "missing_api_key"},
            )
        try:
            client = self._get_client()
            response = await client.messages.create(
                model=model,
                max_tokens=kwargs.get("max_tokens", 4096),
                messages=[{"role": "user", "content": prompt}],
            )
            content = response.content[0].text if response.content else ""
            cache_read = getattr(response.usage, "cache_read_input_tokens", 0) or 0
            cache_created = getattr(response.usage, "cache_creation_input_tokens", 0) or 0
            return ProviderResult(
                provider=self.name,
                model=model,
                content=content,
                metadata={
                    "usage": {
                        "input_tokens": response.usage.input_tokens,
                        "output_tokens": response.usage.output_tokens,
                        "cache_read_input_tokens": cache_read,
                        "cache_creation_input_tokens": cache_created,
                    },
                    "stop_reason": response.stop_reason,
                },
            )
        except Exception as e:
            logger.error("Anthropic generation failed: %s", e)
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
        extended_thinking: bool = False,
    ) -> ProviderResult:
        """Esegue un turno agente con tool_use support nativo Anthropic.

        extended_thinking=True abilita il budget di ragionamento interno (8k token).
        Il parametro e' opt-in: NON attivare per default perche' ogni chiamata
        thinking aggiunge fino a 8000 token di output addebitati a prezzo pieno.
        """
        if not self._api_key:
            return ProviderResult(
                provider=self.name, model=model,
                content="[Anthropic API key not configured]",
                metadata={"error": "missing_api_key"},
            )
        try:
            client = self._get_client()

            # System prompt separato con cache_control ephemeral.
            # Anthropic mette in cache il blocco per 5 min: le chiamate successive
            # riusano la cache al 10% del costo token normale.
            # Se system_text non e' fornito, fallback al vecchio split su marker.
            effective_messages = list(messages)
            system_blocks: list[dict] = []

            if system_text and len(system_text) > 100:
                # Nuovo percorso: system_text separato dal Rust layer
                system_blocks = [{
                    "type": "text",
                    "text": system_text,
                    "cache_control": _system_cache_control(),
                }]
                logger.info(
                    "System prompt separato: %d chars (cached ttl=%s)",
                    len(system_text), _SYSTEM_CACHE_TTL,
                )
            else:
                # Fallback legacy: split su marker per backward compat
                _CACHE_SPLIT_MARKERS = ("Richiesta corrente:", "User request:")
                first = effective_messages[0] if effective_messages else None
                if (
                    first
                    and first.get("role") == "user"
                    and isinstance(first.get("content"), str)
                ):
                    raw = first["content"]
                    split_at = -1
                    for marker in _CACHE_SPLIT_MARKERS:
                        idx = raw.find(marker)
                        if idx != -1:
                            split_at = idx
                            break
                    if split_at > 200:
                        static_part = raw[:split_at].strip()
                        dynamic_part = raw[split_at:].strip()
                        system_blocks = [{
                            "type": "text",
                            "text": static_part,
                            "cache_control": _system_cache_control(),
                        }]
                        effective_messages = [
                            {"role": "user", "content": dynamic_part},
                            *effective_messages[1:],
                        ]

            # ── Compressione tool_result vecchi ───────────────────────────
            # Policy unica di truncation coordinata con il layer Rust (agent_loop.rs):
            #   - Rust fa un first-pass a 60k chars e marca il risultato con NEXUS_TRUNC
            #   - Qui facciamo un soft-pass a MAX_OLD_RESULT_CHARS SOLO sui messaggi
            #     non recenti E solo se NON già marcati da Rust (evita doppio truncation)
            # Aumentato da 400 → 2000 chars: i messaggi "vecchi" mantengono più contesto
            # diagnostico, riducendo la perdita di informazioni su run lunghi.
            KEEP_RECENT = 12             # ultimi N messaggi sempre integrali
            MAX_OLD_RESULT_CHARS = 2000  # chars per tool_result "vecchio" (era 400)
            NEXUS_TRUNC_MARKER = "NEXUS_TRUNC"  # marker del first-pass Rust

            if len(effective_messages) > KEEP_RECENT:
                compressed = []
                cutoff = len(effective_messages) - KEEP_RECENT
                for idx, msg in enumerate(effective_messages):
                    if idx >= cutoff:
                        compressed.append(msg)
                        continue
                    role = msg.get("role")
                    content = msg.get("content")
                    # Comprimi solo i tool_result nei messaggi utente "vecchi"
                    if role == "user" and isinstance(content, list):
                        new_blocks = []
                        changed = False
                        for block in content:
                            btype = block.get("type") if isinstance(block, dict) else None
                            if btype == "tool_result":
                                inner = block.get("content", "")
                                # Può essere lista di blocchi testo o stringa
                                if isinstance(inner, list):
                                    full_text = " ".join(
                                        b.get("text", "") for b in inner
                                        if isinstance(b, dict) and b.get("type") == "text"
                                    )
                                elif isinstance(inner, str):
                                    full_text = inner
                                else:
                                    full_text = str(inner)
                                # Non ritroncare se Rust ha già applicato NEXUS_TRUNC:
                                # preserva il marker e le info originali.
                                already_truncated = NEXUS_TRUNC_MARKER in full_text
                                if not already_truncated and len(full_text) > MAX_OLD_RESULT_CHARS:
                                    truncated = full_text[:MAX_OLD_RESULT_CHARS]
                                    new_block = dict(block)
                                    new_block["content"] = (
                                        f"{truncated}\n[preview: primi {MAX_OLD_RESULT_CHARS} di {len(full_text)} chars]"
                                    )
                                    new_blocks.append(new_block)
                                    changed = True
                                    continue
                            new_blocks.append(block)
                        if changed:
                            new_msg = dict(msg)
                            new_msg["content"] = new_blocks
                            compressed.append(new_msg)
                            continue
                    compressed.append(msg)
                effective_messages = compressed
                logger.debug(
                    "Compressione history: %d → %d msg (tool_result vecchi troncati a %d chars, skip NEXUS_TRUNC)",
                    len(messages), len(effective_messages), MAX_OLD_RESULT_CHARS,
                )

            # ── Prompt cache breakpoints sulla history ─────────────────────
            # Due breakpoint per massimizzare il riuso della cache:
            # - uno a metà della history compressa
            # - uno al terzultimo messaggio user
            if len(effective_messages) >= 6:
                user_indices = [
                    i for i, m in enumerate(effective_messages) if m.get("role") == "user"
                ]

                def _apply_cache_breakpoint(msgs: list, idx: int) -> list:
                    msg = dict(msgs[idx])
                    content = msg.get("content")
                    if isinstance(content, str):
                        msg["content"] = [{
                            "type": "text",
                            "text": content,
                            "cache_control": {"type": "ephemeral"},
                        }]
                    elif isinstance(content, list) and len(content) > 0:
                        new_content = [dict(b) for b in content]
                        last = new_content[-1]
                        if "cache_control" not in last:
                            last["cache_control"] = {"type": "ephemeral"}
                        new_content[-1] = last
                        msg["content"] = new_content
                    result = list(msgs)
                    result[idx] = msg
                    return result

                # Breakpoint 1: terzultimo messaggio user (invariato)
                if len(user_indices) >= 3:
                    effective_messages = _apply_cache_breakpoint(
                        effective_messages, user_indices[-3]
                    )
                    # Breakpoint 2: a metà della history (se ci sono abbastanza messaggi)
                    if len(user_indices) >= 6:
                        mid_idx = user_indices[len(user_indices) // 2]
                        if mid_idx != user_indices[-3]:
                            effective_messages = _apply_cache_breakpoint(
                                effective_messages, mid_idx
                            )

            THINKING_MODELS = {"claude-sonnet-4-6", "claude-opus-4-6", "claude-opus-4-7",
                                "claude-sonnet-4-5", "claude-opus-4-5"}
            # Budget e abilitazione letti dal DB via thinking_config (categoria 'agent').
            # Modificabili dall'admin senza rideploy — TTL cache 60s.
            # Il parametro extended_thinking (legacy) viene IGNORATO: la sorgente
            # di verita' e' esclusivamente il DB per evitare costi imprevisti.
            thinking_budget = _thinking_config.budget_tokens()
            use_thinking = _thinking_config.enabled() and model in THINKING_MODELS and max_tokens > thinking_budget

            kwargs: dict[str, Any] = {
                "model": model,
                "max_tokens": max_tokens,
                "messages": effective_messages,
            }
            if system_blocks:
                kwargs["system"] = system_blocks
            if tools:
                # Comprimi tool defs per ridurre peso JSON inviato (BP6).
                # I campi additionalProperties/$schema/examples/etc vengono rimossi,
                # description tronche a 200 char, enum a 10 valori.
                compressed_tools = compress_tool_list(tools, schema_key="input_schema")
                if logger.isEnabledFor(logging.DEBUG):
                    bytes_before = measure_tools_bytes(tools)
                    bytes_after = measure_tools_bytes(compressed_tools)
                    logger.debug(
                        "anthropic tool_defs compression: %d -> %d bytes (-%.0f%%)",
                        bytes_before, bytes_after,
                        100 * (1 - bytes_after / max(1, bytes_before)),
                    )
                kwargs["tools"] = compressed_tools
                # Anti-narration: al primo turno (nessun tool_result nella history),
                # forza il modello a fare almeno una tool call. Ai turni successivi
                # tool_choice resta auto (default) per permettere risposta testuale.
                # Anthropic usa {"type": "any"} invece di "required".
                from ._schema_utils import is_first_agent_turn
                if is_first_agent_turn(effective_messages):
                    kwargs["tool_choice"] = {"type": "any"}
            if use_thinking:
                kwargs["thinking"] = {"type": "enabled", "budget_tokens": thinking_budget}
                kwargs["betas"] = ["interleaved-thinking-2025-05-14"]
            try:
                response = await client.messages.create(**kwargs)
            except Exception as thinking_err:
                if use_thinking:
                    logger.warning("Extended thinking fallback per %s: %s", model, thinking_err)
                    del kwargs["thinking"]
                    del kwargs["betas"]
                    response = await client.messages.create(**kwargs)
                else:
                    raise
            thinking_text = "".join(
                getattr(b, "thinking", "") for b in response.content
                if getattr(b, "type", None) == "thinking"
            ).strip()
            text_content = next(
                (b.text for b in response.content if getattr(b, "type", None) == "text"), ""
            )
            if thinking_text:
                text_content = f"<nexus:thinking>{thinking_text}</nexus:thinking>\n\n{text_content}"
            tool_use_blocks = [
                {"id": b.id, "name": b.name, "input": b.input}
                for b in response.content
                if getattr(b, "type", None) == "tool_use"
            ]
            # Costruisce il blocco content dell'assistant per la history
            assistant_content: list[dict] = []
            for b in response.content:
                btype = getattr(b, "type", None)
                if btype == "text":
                    assistant_content.append({"type": "text", "text": b.text})
                elif btype == "tool_use":
                    assistant_content.append({
                        "type": "tool_use",
                        "id": b.id,
                        "name": b.name,
                        "input": b.input,
                    })
            usage = response.usage
            cache_read = getattr(usage, "cache_read_input_tokens", 0) or 0
            cache_created = getattr(usage, "cache_creation_input_tokens", 0) or 0
            if cache_read:
                pct = 100.0 * cache_read / max(usage.input_tokens, 1)
                logger.info(
                    "Anthropic cache hit: %d cached / %d input tokens (%.0f%%) risparmio ~90%%",
                    cache_read, usage.input_tokens, pct,
                )
            return ProviderResult(
                provider=self.name,
                model=model,
                content=text_content,
                metadata={
                    "stop_reason": response.stop_reason,
                    "tool_use_blocks": tool_use_blocks,
                    "assistant_content": assistant_content,
                    "usage": {
                        "input_tokens": usage.input_tokens,
                        "output_tokens": usage.output_tokens,
                        "cache_read_tokens": cache_read,
                        "cache_created_tokens": cache_created,
                    },
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
        max_tokens: int = 8192,
        system_text: str = "",
        extended_thinking: bool = False,
    ):
        """Streaming di un turno agente: yielda token parziali poi il risultato finale.

        Yield: {"type": "token", "delta": str} per ogni token
               {"type": "done", "result": dict} al termine (stesso schema di generate_agent_turn)
               {"type": "error", "message": str} in caso di errore
        """
        if not self._api_key:
            yield {"type": "error", "message": "Anthropic API key non configurata"}
            return
        try:
            client = self._get_client()

            # Stessa preparazione di generate_agent_turn: system blocks, compressione, cache
            effective_messages = list(messages)
            system_blocks: list[dict] = []

            if system_text and len(system_text) > 100:
                system_blocks = [{
                    "type": "text",
                    "text": system_text,
                    "cache_control": _system_cache_control(),
                }]
            else:
                _CACHE_SPLIT_MARKERS = ("Richiesta corrente:", "User request:")
                first = effective_messages[0] if effective_messages else None
                if first and first.get("role") == "user" and isinstance(first.get("content"), str):
                    raw = first["content"]
                    split_at = -1
                    for marker in _CACHE_SPLIT_MARKERS:
                        idx = raw.find(marker)
                        if idx != -1:
                            split_at = idx
                            break
                    if split_at > 200:
                        system_blocks = [{"type": "text", "text": raw[:split_at].strip(), "cache_control": _system_cache_control()}]
                        effective_messages = [{"role": "user", "content": raw[split_at:].strip()}, *effective_messages[1:]]

            KEEP_RECENT = 12
            MAX_OLD_RESULT_CHARS = 400
            if len(effective_messages) > KEEP_RECENT:
                compressed = []
                cutoff = len(effective_messages) - KEEP_RECENT
                for idx, msg in enumerate(effective_messages):
                    if idx >= cutoff:
                        compressed.append(msg)
                        continue
                    role = msg.get("role")
                    content = msg.get("content")
                    if role == "user" and isinstance(content, list):
                        new_blocks = []
                        changed = False
                        for block in content:
                            btype = block.get("type") if isinstance(block, dict) else None
                            if btype == "tool_result":
                                inner = block.get("content", "")
                                if isinstance(inner, list):
                                    full_text = " ".join(b.get("text", "") for b in inner if isinstance(b, dict) and b.get("type") == "text")
                                elif isinstance(inner, str):
                                    full_text = inner
                                else:
                                    full_text = str(inner)
                                if len(full_text) > MAX_OLD_RESULT_CHARS:
                                    new_block = dict(block)
                                    new_block["content"] = f"{full_text[:MAX_OLD_RESULT_CHARS]}…[troncato: {len(full_text)} chars]"
                                    new_blocks.append(new_block)
                                    changed = True
                                    continue
                            new_blocks.append(block)
                        if changed:
                            new_msg = dict(msg)
                            new_msg["content"] = new_blocks
                            compressed.append(new_msg)
                            continue
                    compressed.append(msg)
                effective_messages = compressed

            if len(effective_messages) >= 6:
                user_indices = [i for i, m in enumerate(effective_messages) if m.get("role") == "user"]

                def _apply_cache_breakpoint(msgs: list, idx: int) -> list:
                    msg = dict(msgs[idx])
                    content = msg.get("content")
                    if isinstance(content, str):
                        msg["content"] = [{"type": "text", "text": content, "cache_control": {"type": "ephemeral"}}]
                    elif isinstance(content, list) and len(content) > 0:
                        new_content = [dict(b) for b in content]
                        last = new_content[-1]
                        if "cache_control" not in last:
                            last["cache_control"] = {"type": "ephemeral"}
                        new_content[-1] = last
                        msg["content"] = new_content
                    result = list(msgs)
                    result[idx] = msg
                    return result

                if len(user_indices) >= 3:
                    effective_messages = _apply_cache_breakpoint(effective_messages, user_indices[-3])
                    if len(user_indices) >= 6:
                        mid_idx = user_indices[len(user_indices) // 2]
                        if mid_idx != user_indices[-3]:
                            effective_messages = _apply_cache_breakpoint(effective_messages, mid_idx)

            # Budget e abilitazione letti dal DB via thinking_config (categoria 'agent').
            # Stessa logica del path non-streaming: sorgente di verita' e' il DB.
            THINKING_MODELS = {"claude-sonnet-4-6", "claude-opus-4-6", "claude-opus-4-7",
                                "claude-sonnet-4-5", "claude-opus-4-5"}
            thinking_budget = _thinking_config.budget_tokens()
            use_thinking = _thinking_config.enabled() and model in THINKING_MODELS and max_tokens > thinking_budget

            stream_kwargs: dict[str, Any] = {
                "model": model,
                "max_tokens": max_tokens,
                "messages": effective_messages,
            }
            if system_blocks:
                stream_kwargs["system"] = system_blocks
            if tools:
                # Stessa compressione del path non-streaming (BP6).
                stream_kwargs["tools"] = compress_tool_list(tools, schema_key="input_schema")
            if use_thinking:
                stream_kwargs["thinking"] = {"type": "enabled", "budget_tokens": thinking_budget}
                stream_kwargs["betas"] = ["interleaved-thinking-2025-05-14"]

            thinking_parts: list[str] = []

            try:
                async with client.messages.stream(**stream_kwargs) as stream:
                    async for event in stream:
                        etype = getattr(event, "type", None)
                        if etype == "content_block_delta":
                            delta = getattr(event, "delta", None)
                            delta_type = getattr(delta, "type", None)
                            if delta_type == "thinking_delta":
                                thinking_parts.append(getattr(delta, "thinking", ""))
                            elif delta_type == "text_delta":
                                yield {"type": "token", "delta": getattr(delta, "text", "")}
                    final = await stream.get_final_message()
            except Exception as thinking_err:
                if use_thinking:
                    # Fallback senza thinking se il modello non supporta il beta
                    logger.warning("Extended thinking fallback per %s: %s", model, thinking_err)
                    del stream_kwargs["thinking"]
                    del stream_kwargs["betas"]
                    use_thinking = False
                    thinking_parts = []
                    async with client.messages.stream(**stream_kwargs) as stream:
                        async for text in stream.text_stream:
                            yield {"type": "token", "delta": text}
                        final = await stream.get_final_message()
                else:
                    raise

            # Costruisce il risultato finale con lo stesso schema di generate_agent_turn
            text_content = next((b.text for b in final.content if hasattr(b, "text")), "")
            thinking_text = "".join(thinking_parts).strip()
            if thinking_text:
                text_content = f"<nexus:thinking>{thinking_text}</nexus:thinking>\n\n{text_content}"
            tool_use_blocks = [
                {"id": b.id, "name": b.name, "input": b.input}
                for b in final.content if getattr(b, "type", None) == "tool_use"
            ]
            assistant_content: list[dict] = []
            for b in final.content:
                btype = getattr(b, "type", None)
                if btype == "text":
                    assistant_content.append({"type": "text", "text": b.text})
                elif btype == "tool_use":
                    assistant_content.append({"type": "tool_use", "id": b.id, "name": b.name, "input": b.input})
            usage = final.usage
            cache_read = getattr(usage, "cache_read_input_tokens", 0) or 0
            cache_created = getattr(usage, "cache_creation_input_tokens", 0) or 0
            yield {
                "type": "done",
                "result": {
                    "provider": self.name,
                    "model": model,
                    "content": text_content,
                    "metadata": {
                        "stop_reason": final.stop_reason,
                        "tool_use_blocks": tool_use_blocks,
                        "assistant_content": assistant_content,
                        "usage": {
                            "input_tokens": usage.input_tokens,
                            "output_tokens": usage.output_tokens,
                            "cache_read_tokens": cache_read,
                            "cache_created_tokens": cache_created,
                        },
                    },
                },
            }
        except Exception as e:
            meta = format_error_result(e, self.name, model)
            yield {"type": "error", "message": meta.get("error", str(e)), "metadata": meta}

    async def generate_stream(self, model: str, prompt: str, **kwargs: Any) -> AsyncIterator[str]:
        if not self._api_key:
            yield "[Anthropic API key not configured]"
            return
        try:
            client = self._get_client()
            async with client.messages.stream(
                model=model,
                max_tokens=kwargs.get("max_tokens", 4096),
                messages=[{"role": "user", "content": prompt}],
            ) as stream:
                async for text in stream.text_stream:
                    yield text
        except Exception as e:
            logger.error("Anthropic stream failed: %s", e)
            yield f"[Error: {e}]"

    async def test_connection(self) -> dict[str, Any]:
        if not self._api_key:
            return {"provider": self.name, "status": "not_configured", "reason": "API key non configurata"}
        try:
            client = self._get_client()
            await client.messages.create(
                model="claude-haiku-4-5-20251001",
                max_tokens=10,
                messages=[{"role": "user", "content": "ping"}],
            )
            return {"provider": self.name, "status": "ready"}
        except Exception as e:
            from .error_handler import classify_error
            info = classify_error(e, self.name)
            return {"provider": self.name, "status": "error", "reason": info["message"], "error_class": info["stop_reason"]}
