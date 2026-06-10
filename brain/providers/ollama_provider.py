"""Provider locale Ollama — modelli on-premise senza dipendenze cloud.

Supporta qualsiasi modello installato in Ollama (ollama.ai):
- DeepSeek-R1 distill (privato, nessun dato cloud)
- Qwen 2.5 Coder (coding locale)
- Llama 3.x (universale)

Configurazione:
  OLLAMA_URL=http://localhost:11434   (default)
  OLLAMA_ENABLED=true|false           (default: true se URL raggiungibile)
"""
from __future__ import annotations

import logging
import os
from typing import Any, AsyncIterator

import httpx

from .base import BaseProvider, ProviderCatalogEntry, ProviderResult

logger = logging.getLogger(__name__)

_DEFAULT_OLLAMA_URL = "http://localhost:11434"


class OllamaProvider(BaseProvider):
    """Provider locale tramite Ollama — zero privacy risk, dati mai fuori dalla macchina."""

    name = "ollama"

    def __init__(self) -> None:
        self._base_url = os.getenv("OLLAMA_URL", _DEFAULT_OLLAMA_URL).rstrip("/")
        self._client: httpx.AsyncClient | None = None

    def _get_client(self) -> httpx.AsyncClient:
        if self._client is None:
            self._client = httpx.AsyncClient(
                base_url=self._base_url,
                timeout=httpx.Timeout(120.0, connect=5.0),
            )
        return self._client

    def list_models(self) -> list[ProviderCatalogEntry]:
        """Modelli predefiniti — la lista reale si ottiene via /api/tags a runtime."""
        return [
            ProviderCatalogEntry("deepseek-r1:7b",  ["chat", "reasoning", "local"]),
            ProviderCatalogEntry("deepseek-r1:14b", ["chat", "reasoning", "local"]),
            ProviderCatalogEntry("deepseek-r1:32b", ["chat", "reasoning", "local"]),
            ProviderCatalogEntry("qwen2.5-coder:7b",  ["chat", "coding", "local"]),
            ProviderCatalogEntry("qwen2.5-coder:14b", ["chat", "coding", "local"]),
            ProviderCatalogEntry("qwen2.5-coder:32b", ["chat", "coding", "local"]),
            ProviderCatalogEntry("llama3.2:3b",  ["chat", "local", "fast"]),
            ProviderCatalogEntry("llama3.2:8b",  ["chat", "local"]),
            ProviderCatalogEntry("llama3.1:70b", ["chat", "local", "large"]),
        ]

    async def _list_running_models(self) -> list[str]:
        """Recupera i modelli installati da Ollama."""
        try:
            resp = await self._get_client().get("/api/tags", timeout=3.0)
            resp.raise_for_status()
            data = resp.json()
            return [m["name"] for m in data.get("models", [])]
        except Exception:
            return []

    async def test_connection(self) -> dict[str, Any]:
        try:
            resp = await self._get_client().get("/api/tags", timeout=3.0)
            resp.raise_for_status()
            data = resp.json()
            models = [m["name"] for m in data.get("models", [])]
            return {
                "provider": self.name,
                "status": "ok",
                "models": models,
                "url": self._base_url,
            }
        except httpx.ConnectError:
            return {"provider": self.name, "status": "offline", "reason": f"Ollama non raggiungibile su {self._base_url}"}
        except Exception as e:
            return {"provider": self.name, "status": "error", "reason": str(e)}

    async def generate(self, model: str, prompt: str, **kwargs: Any) -> ProviderResult:
        try:
            client = self._get_client()
            payload = {
                "model": model,
                "prompt": prompt,
                "stream": False,
                "options": {
                    "temperature": kwargs.get("temperature", 0.7),
                    "num_predict": kwargs.get("max_tokens", 4096),
                },
            }
            resp = await client.post("/api/generate", json=payload, timeout=120.0)
            resp.raise_for_status()
            data = resp.json()
            return ProviderResult(
                provider=self.name,
                model=model,
                content=data.get("response", ""),
                metadata={
                    "done": data.get("done", True),
                    "eval_count": data.get("eval_count", 0),
                    "prompt_eval_count": data.get("prompt_eval_count", 0),
                },
            )
        except Exception as e:
            # Contratto dati B (regola L): error_class + http_status strutturati
            # dall'oggetto SDK reale (niente fallback lessicale a valle).
            from .error_handler import format_error_result
            meta = format_error_result(e, self.name, model)
            return ProviderResult(
                provider=self.name, model=model,
                content=f"[Ollama Error: {meta['error']}]",
                metadata=meta,
            )

    async def generate_stream(self, model: str, prompt: str, **kwargs: Any) -> AsyncIterator[str]:
        try:
            async with self._get_client().stream(
                "POST", "/api/generate",
                json={
                    "model": model,
                    "prompt": prompt,
                    "stream": True,
                    "options": {"temperature": kwargs.get("temperature", 0.7)},
                },
                timeout=120.0,
            ) as resp:
                resp.raise_for_status()
                import json as _json
                async for line in resp.aiter_lines():
                    if line:
                        try:
                            chunk = _json.loads(line)
                            if text := chunk.get("response"):
                                yield text
                            if chunk.get("done"):
                                break
                        except _json.JSONDecodeError:
                            continue
        except Exception as e:
            yield f"[Ollama stream error: {e}]"

    async def generate_agent_turn(
        self,
        model: str,
        messages: list[dict],
        tools: list[dict],
        max_tokens: int = 4096,
        system_text: str = "",
        **kwargs: Any,
    ) -> ProviderResult:
        """
        Agent turn tramite Ollama chat API.
        Nota: Ollama supporta tool_use solo per modelli recenti (llama3.2, qwen2.5, etc.).
        Per modelli senza tool_use nativo, i tool vengono iniettati nel system prompt.
        """
        try:
            client = self._get_client()

            # Costruisci messages Ollama (simile a OpenAI)
            ollama_messages: list[dict] = []
            if system_text:
                if tools:
                    # Inietta i tool nel system prompt per modelli senza tool_use nativo
                    import json as _json
                    tools_desc = "\n".join(
                        f"- {t.get('name', t.get('function', {}).get('name', '?'))}: "
                        f"{t.get('description', t.get('function', {}).get('description', ''))}"
                        for t in tools
                    )
                    system_text += (
                        f"\n\nTool disponibili:\n{tools_desc}\n\n"
                        "Per usare un tool, rispondi con: TOOL: <nome_tool>(<parametri_json>)"
                    )
                ollama_messages.append({"role": "system", "content": system_text})

            ollama_messages.extend(messages)

            temperature = kwargs.get("temperature", 0.7)
            payload = {
                "model": model,
                "messages": ollama_messages,
                "stream": False,
                "options": {"num_predict": max_tokens, "temperature": temperature},
            }

            # Aggiungi tool definition per modelli che supportano tool_use
            if tools:
                payload["tools"] = [
                    {
                        "type": "function",
                        "function": {
                            "name": t.get("name", t.get("function", {}).get("name")),
                            "description": t.get("description", t.get("function", {}).get("description", "")),
                            "parameters": t.get("input_schema", t.get("function", {}).get("parameters", {})),
                        }
                    }
                    for t in tools
                ]

            resp = await client.post("/api/chat", json=payload, timeout=120.0)
            resp.raise_for_status()
            data = resp.json()

            msg = data.get("message", {})
            content_text = msg.get("content", "")
            tool_calls_raw = msg.get("tool_calls", [])

            # Normalizza tool_calls al formato standard Anthropic (tool_use_blocks)
            tool_use_blocks: list[dict] = []
            assistant_content: list[dict] = []
            for tc in tool_calls_raw:
                fn = tc.get("function", {})
                block = {
                    "id": tc.get("id", f"ollama_{fn.get('name', 'tool')}"),
                    "name": fn.get("name"),
                    "input": fn.get("arguments", {}),
                }
                tool_use_blocks.append(block)
                assistant_content.append({"type": "tool_use", **block})

            if not tool_use_blocks and content_text:
                assistant_content.append({"type": "text", "text": content_text})

            stop_reason = "end_turn"
            if tool_use_blocks:
                stop_reason = "tool_use"
            elif data.get("done_reason") == "length":
                stop_reason = "max_tokens"

            # Normalizza usage al formato standard (input_tokens / output_tokens)
            eval_count = data.get("eval_count", 0)
            prompt_eval_count = data.get("prompt_eval_count", 0)

            return ProviderResult(
                provider=self.name,
                model=model,
                content=content_text,
                metadata={
                    "stop_reason": stop_reason,
                    "tool_use_blocks": tool_use_blocks,
                    "assistant_content": assistant_content,
                    "usage": {
                        "input_tokens": prompt_eval_count,
                        "output_tokens": eval_count,
                    },
                },
            )

        except Exception as e:
            logger.error("Ollama agent turn failed model=%s: %s", model, e)
            return ProviderResult(
                provider=self.name, model=model,
                content=f"[Ollama Error: {e}]",
                metadata={"error": str(e), "stop_reason": "error"},
            )
