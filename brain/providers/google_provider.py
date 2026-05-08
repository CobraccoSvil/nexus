"""Google Gemini provider — usa il nuovo SDK google-genai (v1.x)."""
from __future__ import annotations

import logging
import os
from typing import Any, AsyncIterator

from .base import BaseProvider, ProviderCatalogEntry, ProviderResult
from .error_handler import format_error_result

logger = logging.getLogger(__name__)


class GoogleProvider(BaseProvider):
    name = "google"

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
            from google import genai  # type: ignore[import]
            from .dns_transport import get_global_dns_transport
            transport = get_global_dns_transport()
            if transport is not None:
                # Google genai non supporta http_client custom; usiamo monkey-patch socket
                import socket as _socket
                import dns.resolver as _dns
                _resolver = _dns.Resolver(configure=False)
                _resolver.nameservers = transport._dns_resolver.nameservers
                _original = getattr(_socket, '_orig_getaddrinfo', _socket.getaddrinfo)
                _socket._orig_getaddrinfo = _original
                def _custom_gai(host, port, family=0, type=0, proto=0, flags=0):
                    # IMPORTANTE: getaddrinfo puo' essere chiamato con host=bytes
                    # (es. da urllib3/httpx in alcuni code path); inet_pton invece
                    # accetta SOLO str. Senza la coercion qui sotto si rompe con
                    # "TypeError: inet_pton() argument 2 must be str, not bytes"
                    # (vedi bug 6 del test E2E redemptor).
                    host_str = host.decode('ascii', errors='ignore') if isinstance(host, (bytes, bytearray)) else host
                    for af in (_socket.AF_INET, _socket.AF_INET6):
                        try:
                            _socket.inet_pton(af, host_str)
                            return _original(host, port, family, type, proto, flags)
                        except (_socket.error, TypeError):
                            pass
                    try:
                        return _original(str(_resolver.resolve(host_str, 'A')[0]), port, family, type, proto, flags)
                    except Exception:
                        return _original(host, port, family, type, proto, flags)
                _socket.getaddrinfo = _custom_gai
            self._client = genai.Client(api_key=self._api_key)
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
                content="[Google API key not configured]",
                metadata={"error": "missing_api_key"},
            )
        try:
            from google.genai import types  # type: ignore[import]
            client = self._get_client()
            response = await client.aio.models.generate_content(
                model=model,
                contents=prompt,
                config=types.GenerateContentConfig(
                    max_output_tokens=kwargs.get("max_tokens", 4096),
                    temperature=kwargs.get("temperature", 0.7),
                ),
            )
            prompt_tokens = 0
            completion_tokens = 0
            if response.usage_metadata:
                prompt_tokens = response.usage_metadata.prompt_token_count or 0
                completion_tokens = response.usage_metadata.candidates_token_count or 0
            return ProviderResult(
                provider=self.name,
                model=model,
                content=response.text or "",
                metadata={
                    "usage": {
                        "prompt_tokens": prompt_tokens,
                        "completion_tokens": completion_tokens,
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

    async def generate_agent_turn(
        self,
        model: str,
        messages: list[dict],
        tools: list[dict],
        max_tokens: int = 4096,
        system_text: str = "",
    ) -> ProviderResult:
        """Turno agente con function calling Google Gemini, normalizzato al formato Anthropic."""
        if not self._api_key:
            return ProviderResult(
                provider=self.name, model=model,
                content="[Google API key not configured]",
                metadata={"error": "missing_api_key"},
            )
        try:
            from google.genai import types  # type: ignore[import]
            import json as _json

            client = self._get_client()

            # Converti messaggi Anthropic -> Google genai Contents
            contents = _convert_messages_to_google(messages)

            # Converti tool definitions Anthropic -> Google FunctionDeclaration
            google_tools = None
            if tools:
                func_decls = []
                for t in tools:
                    schema = t.get("input_schema", {"type": "object", "properties": {}})
                    # Rimuovi chiavi non supportate da Google (additionalProperties, $schema)
                    clean_schema = _clean_schema_for_google(schema)
                    func_decls.append(types.FunctionDeclaration(
                        name=t["name"],
                        description=t.get("description", ""),
                        parameters=clean_schema,
                    ))
                google_tools = [types.Tool(function_declarations=func_decls)]

            config = types.GenerateContentConfig(
                max_output_tokens=max_tokens,
                temperature=0.3,
                tools=google_tools,
            )
            if system_text:
                config.system_instruction = system_text

            response = await client.aio.models.generate_content(
                model=model,
                contents=contents,
                config=config,
            )

            # Normalizza risposta al formato Anthropic
            text_content = ""
            stop_reason = "end_turn"
            tool_use_blocks: list[dict] = []
            assistant_content: list[dict] = []

            if response.candidates and response.candidates[0].content and response.candidates[0].content.parts:
                for part in response.candidates[0].content.parts:
                    if part.text:
                        text_content += part.text
                    elif part.function_call:
                        stop_reason = "tool_use"
                        fc = part.function_call
                        # Genera un ID univoco per il tool_use block
                        import uuid
                        tool_id = f"toolu_{uuid.uuid4().hex[:24]}"
                        args = dict(fc.args) if fc.args else {}
                        block = {"id": tool_id, "name": fc.name, "input": args}
                        tool_use_blocks.append(block)
                        assistant_content.append({"type": "tool_use", **block})

            if not tool_use_blocks and text_content:
                assistant_content.append({"type": "text", "text": text_content})

            # Usage
            usage_data = {}
            if response.usage_metadata:
                usage_data = {
                    "input_tokens": response.usage_metadata.prompt_token_count or 0,
                    "output_tokens": response.usage_metadata.candidates_token_count or 0,
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
            yield "[Google API key not configured]"
            return
        try:
            from google.genai import types  # type: ignore[import]
            client = self._get_client()
            async for chunk in await client.aio.models.generate_content_stream(
                model=model,
                contents=prompt,
                config=types.GenerateContentConfig(
                    max_output_tokens=kwargs.get("max_tokens", 4096),
                    temperature=kwargs.get("temperature", 0.7),
                ),
            ):
                if chunk.text:
                    yield chunk.text
        except Exception as e:
            logger.error("Google stream failed: %s", e)
            yield f"[Error: {e}]"

    async def test_connection(self) -> dict[str, Any]:
        if not self._api_key:
            return {"provider": self.name, "status": "not_configured", "reason": "API key non configurata"}
        try:
            client = self._get_client()
            # Usa models.list() (non fatturata) invece di generate_content per evitare
            # di consumare crediti ad ogni health-check.
            async for _ in client.aio.models.list():
                break
            return {"provider": self.name, "status": "ready"}
        except Exception as e:
            from .error_handler import classify_error
            info = classify_error(e, self.name)
            return {"provider": self.name, "status": "error", "reason": info["message"], "error_class": info["stop_reason"]}


def _clean_schema_for_google(schema: dict) -> dict:
    """Rimuovi chiavi non supportate da Google genai e applica compressione (BP6).

    Delega al modulo condiviso _schema_utils.compress_schema che rimuove
    additionalProperties/$schema/default/examples/title, tronca description
    a 200 char e enum a 10 valori. Backward compatible con callers esistenti.
    """
    from ._schema_utils import compress_schema
    return compress_schema(schema)


def _convert_messages_to_google(messages: list[dict]) -> list[Any]:
    """Converte messaggi formato Anthropic (con tool_use/tool_result) in formato Google genai Contents."""
    from google.genai import types  # type: ignore[import]

    # Mappa tool_use_id -> tool_name per risolvere i tool_result
    id_to_name: dict[str, str] = {}
    for msg in messages:
        content = msg.get("content", "")
        if isinstance(content, list):
            for block in content:
                if block.get("type") == "tool_use":
                    id_to_name[block.get("id", "")] = block["name"]

    contents: list[Any] = []
    for msg in messages:
        role = msg.get("role", "user")
        # Google usa "user" e "model" (non "assistant")
        g_role = "model" if role == "assistant" else "user"
        content = msg.get("content", "")

        if isinstance(content, str):
            contents.append(types.Content(
                role=g_role,
                parts=[types.Part.from_text(text=content)],
            ))
        elif isinstance(content, list):
            parts: list[Any] = []
            tool_response_parts: list[Any] = []
            for block in content:
                btype = block.get("type")
                if btype == "text":
                    text_val = block.get("text", "")
                    if text_val:
                        parts.append(types.Part.from_text(text=text_val))
                elif btype == "tool_use":
                    # Blocco tool_use (assistant chiede di chiamare un tool)
                    parts.append(types.Part.from_function_call(
                        name=block["name"],
                        args=block.get("input", {}),
                    ))
                elif btype == "tool_result":
                    # Blocco tool_result — Google vuole il name del tool, non l'id
                    result_content = block.get("content", "")
                    if isinstance(result_content, list):
                        result_content = " ".join(
                            b.get("text", "") for b in result_content if b.get("type") == "text"
                        )
                    tool_use_id = block.get("tool_use_id", "")
                    tool_name = id_to_name.get(tool_use_id, tool_use_id)
                    tool_response_parts.append(types.Part.from_function_response(
                        name=tool_name,
                        response={"result": str(result_content)},
                    ))

            if tool_response_parts:
                # I tool_result vanno come messaggio "user" separato
                contents.append(types.Content(
                    role="user",
                    parts=tool_response_parts,
                ))
            if parts:
                contents.append(types.Content(
                    role=g_role,
                    parts=parts,
                ))
        else:
            contents.append(types.Content(
                role=g_role,
                parts=[types.Part.from_text(text=str(content))],
            ))

    return contents
