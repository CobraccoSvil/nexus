"""Provider che delega al gateway LLM Rust (crates/nexus-gateway) via HTTP.

Questo provider NON parla con gli SDK dei vendor: inoltra la richiesta al
gateway Rust (POST /v1/complete e /v1/stream) che possiede la logica di routing,
cooldown e privacy. E' il primo passo della migrazione che, a regime, sostituira'
gli adapter SDK Python (deepseek/mistral/openai/anthropic/google) eliminando la
duplicazione provider Python<->Rust.

Stato: il GatewayProvider ESISTE ed e' costruibile/testabile, ma il registry NON
lo usa di default (nessuno switch in questa fase).

Contratto del gateway (vedi crates/nexus-gateway/src/types.rs):
  - POST /v1/complete -> LlmResponse{content, reasoning?, thinking_signature?,
    tool_calls?, usage{input_tokens, output_tokens, cache_read_tokens?,
    cache_creation_tokens?}, model_used, provider_used, finish_reason, latency_ms}
  - POST /v1/stream -> SSE di LlmStreamChunk{delta, reasoning_delta?,
    tool_call_delta?, finish_reason?, usage?, ...}, terminato da `data: [DONE]`.
  - Auth: header `Authorization: Bearer <service_token>`.

Lingua franca del gateway: OpenAI Chat Completions. I messaggi interni del brain
sono in formato Anthropic-like (blocchi type=text/tool_use/tool_result/reasoning):
la conversione verso il dialetto OpenAI usa il PUNTO UNICO esistente
``adapter_base.convert_messages_to_openai`` (regola L), evitando di re-implementare
la traduzione. La risposta del gateway (tool_calls in dialetto OpenAI) viene
ri-normalizzata al formato Anthropic interno (tool_use_blocks + assistant_content)
atteso da ``BaseProvider.generate_agent_turn``.

Regola G: URL e token NON sono mai hardcoded come default magici di business.
URL del gateway: env ``NEXUS_GATEWAY_URL`` (override) > setting DB
``nexus_gateway_url`` > fallback locale dev. Token: env
``NEXUS_GATEWAY_SERVICE_TOKEN`` con lo stesso fallback dev del gateway Rust.

Regola F: nessun prompt/response in chiaro nei log (solo lunghezze, conteggi,
finish_reason, status code).
"""
from __future__ import annotations

import json
import logging
import os
from collections.abc import AsyncIterator
from typing import Any

import httpx

from .base import BaseProvider, ProviderCatalogEntry, ProviderResult

logger = logging.getLogger(__name__)

# Fallback dev del service token: DEVE coincidere con il default del gateway Rust
# (crates/nexus-gateway) per far funzionare l'ambiente locale senza configurazione.
# Non e' un "magic fallback" di business (regola G): e' la credenziale di sviluppo
# documentata, sovrascritta in produzione da NEXUS_GATEWAY_SERVICE_TOKEN.
_DEV_SERVICE_TOKEN = "dev-internal-token"

# Fallback locale dell'URL gateway (porta 4060, vedi deploy). Override via env o DB.
_DEV_GATEWAY_URL = "http://127.0.0.1:4060"

# Timeout HTTP (secondi). DB-driven con fallback prudente; il gateway puo' a sua
# volta attendere un provider lento, quindi teniamo un margine ampio.
_DEFAULT_COMPLETE_TIMEOUT_S = 120.0
_DEFAULT_STREAM_TIMEOUT_S = 300.0

# Tenant/user placeholder per le richieste senza contesto utente sul canale gRPC
# del brain (stesso razionale di registry._billing_context). Il gateway richiede
# i campi metadata; usiamo l'UUID di sistema invece di stringhe vuote.
_SYSTEM_UUID = "00000000-0000-0000-0000-000000000000"

# Provider noti, coerenti coi nomi costruiti come LlmProvider lato gateway Rust
# (crates/nexus-gateway/src/providers/*: fn name() -> "openai"|"anthropic"|...).
# Quando il model arriva nel formato "<provider>/<model>" e il prefisso e' uno di
# questi, lo splittiamo in pin_provider + model concreto: cosi' il gateway esegue
# ESATTAMENTE quel provider (path pin) senza ri-fare il routing per-tier ne' il
# fallback cross-provider (gli alias "-fallback"). Un prefisso NON noto (es. un
# nome di modello che contiene "/" ma non e' un provider) non attiva il pin: il
# model resta invariato e il gateway segue il routing storico.
_KNOWN_GATEWAY_PROVIDERS = frozenset(
    {"openai", "anthropic", "mistral", "deepseek", "google", "vllm"}
)


def _split_pin_provider(model: str) -> tuple[str | None, str]:
    """Splitta un model ``"<provider>/<model>"`` in ``(pin_provider, model)``.

    Ritorna ``(provider, resto)`` solo se la prima componente prima del primo
    ``/`` e' un provider noto del gateway (``_KNOWN_GATEWAY_PROVIDERS``). In tutti
    gli altri casi (nessun ``/``, prefisso non riconosciuto, componente vuota)
    ritorna ``(None, model)`` invariato: nessun pin, comportamento storico.

    Lo split usa ``split_once`` semantico (solo la PRIMA ``/``), coerente con
    ``strip_model_prefix`` del gateway: un model con piu' segmenti (es.
    ``vllm/org/modello``) mantiene il resto intatto come modello concreto.
    """
    prefix, sep, rest = model.partition("/")
    if not sep or not rest:
        return None, model
    if prefix.lower() in _KNOWN_GATEWAY_PROVIDERS:
        return prefix.lower(), rest
    return None, model


def _gateway_url() -> str:
    """URL base del gateway LLM Rust.

    Ordine (regola G): env ``NEXUS_GATEWAY_URL`` (override emergenza) > setting
    DB ``nexus_gateway_url`` (canonico) > fallback locale dev. Best-effort sulla
    lettura DB: se il DB e' down si usa il fallback locale (il gateway gira
    sullo stesso host in sviluppo).
    """
    env = os.environ.get("NEXUS_GATEWAY_URL")
    if env:
        return env.rstrip("/")
    try:
        from brain.utils.settings_db import get_setting
        return get_setting("nexus_gateway_url", _DEV_GATEWAY_URL).rstrip("/")
    except Exception:  # noqa: BLE001
        return _DEV_GATEWAY_URL


def _service_token() -> str:
    """Service token per l'header Authorization.

    Da env ``NEXUS_GATEWAY_SERVICE_TOKEN`` con lo stesso fallback dev del gateway
    Rust (``dev-internal-token``). In produzione l'env e' impostato dal deploy.
    """
    return os.environ.get("NEXUS_GATEWAY_SERVICE_TOKEN", _DEV_SERVICE_TOKEN)


def _complete_timeout_s() -> float:
    try:
        from brain.utils.settings_db import get_int_setting
        return float(get_int_setting("gateway.complete_timeout_seconds", int(_DEFAULT_COMPLETE_TIMEOUT_S)))
    except Exception:  # noqa: BLE001
        return _DEFAULT_COMPLETE_TIMEOUT_S


def _stream_timeout_s() -> float:
    try:
        from brain.utils.settings_db import get_int_setting
        return float(get_int_setting("gateway.stream_timeout_seconds", int(_DEFAULT_STREAM_TIMEOUT_S)))
    except Exception:  # noqa: BLE001
        return _DEFAULT_STREAM_TIMEOUT_S


def _build_metadata(feature: str) -> dict[str, Any]:
    """Costruisce il blocco metadata richiesto dal gateway (RequestMetadata).

    Il canale gRPC del brain non trasporta tenant/user reali: usiamo l'UUID di
    sistema (come registry._billing_context) per soddisfare il contratto senza
    inventare dati. ``request_id`` e' un id opaco per il tracing lato gateway.
    """
    import uuid as _uuid
    return {
        "tenant_id": _SYSTEM_UUID,
        "user_id": _SYSTEM_UUID,
        "request_id": str(_uuid.uuid4()),
        "sensitivity_tier": 0,
        "feature": feature,
    }


def _to_gateway_messages(
    messages: list[dict], system_text: str = ""
) -> list[dict]:
    """Converte i messaggi interni del brain (Anthropic-like) nei LlmMessage del
    gateway (dialetto OpenAI).

    Riusa il PUNTO UNICO ``adapter_base.convert_messages_to_openai`` (regola L)
    per la traduzione dei blocchi (tool_use -> tool_calls, tool_result ->
    messaggio role=tool, reasoning -> reasoning_content). Il ``system_text``, se
    presente, viene anteposto come messaggio role=system (il gateway accetta il
    ruolo system nel dialetto OpenAI).

    Il ``reasoning_content`` eventualmente prodotto dalla conversione viene
    rimosso (il gateway non lo accetta nello schema LlmMessage): il round-trip
    del thinking avviene via ``thinking_signature`` sul messaggio assistant
    (mappato a partire dal blocco thinking nativo, vedi ``_attach_thinking_signature``).
    """
    from .adapter_base import convert_messages_to_openai

    oai = convert_messages_to_openai(messages)
    gw: list[dict] = []
    if system_text:
        gw.append({"role": "system", "content": system_text})
    for m in oai:
        msg: dict[str, Any] = {"role": m.get("role", "user")}
        # content puo' essere None (assistant con sole tool_calls): il gateway
        # vuole una stringa o lista di blocchi -> normalizziamo a stringa vuota.
        content = m.get("content")
        msg["content"] = content if content is not None else ""
        if m.get("tool_calls"):
            msg["tool_calls"] = m["tool_calls"]
        if m.get("tool_call_id"):
            msg["tool_call_id"] = m["tool_call_id"]
        gw.append(msg)
    _attach_thinking_signature(messages, gw)
    return gw


def _attach_thinking_signature(internal_messages: list[dict], gw_messages: list[dict]) -> None:
    """Propaga la ``thinking_signature`` dei blocchi thinking Anthropic sui
    corrispondenti messaggi assistant del gateway.

    Il formato interno conserva il blocco ``{"type": "thinking", "signature":
    ...}`` in coda all'assistant_content (vedi anthropic_provider). Il gateway lo
    richiede come campo ``thinking_signature`` di LlmMessage per ri-passarlo nei
    turni con tool (HTTP 400 Anthropic altrimenti). Mappiamo per posizione tra i
    messaggi assistant interni e quelli del gateway che li rappresentano.
    """
    # Estrai le signature dai messaggi assistant interni, nell'ordine.
    sigs: list[str] = []
    for msg in internal_messages:
        if msg.get("role") != "assistant":
            continue
        content = msg.get("content")
        sig = None
        if isinstance(content, list):
            for block in content:
                if isinstance(block, dict) and block.get("type") == "thinking":
                    sig = block.get("signature")
                    break
        sigs.append(sig or "")
    # Applica in ordine ai messaggi assistant del gateway. La conversione OpenAI
    # puo' spezzare un assistant in piu' messaggi solo per i tool_result (che
    # diventano role=tool, non assistant): l'ordine degli assistant si conserva.
    idx = 0
    for gm in gw_messages:
        if gm.get("role") != "assistant":
            continue
        if idx < len(sigs) and sigs[idx]:
            gm["thinking_signature"] = sigs[idx]
        idx += 1


def _to_gateway_tools(tools: list[dict]) -> list[dict]:
    """Converte i tool definition Anthropic-like del brain nel formato
    LlmToolDefinition del gateway (dialetto OpenAI: {type, function{...}}).

    Riusa il punto unico ``adapter_base.anthropic_tool_to_openai`` (regola L),
    che produce gia' ``{"type": "function", "function": {name, description,
    parameters}}`` — identico allo schema LlmToolDefinition del gateway.
    """
    from .adapter_base import anthropic_tool_to_openai

    return [anthropic_tool_to_openai(t) for t in tools if t.get("name")]


def _tool_calls_to_blocks(tool_calls: list[dict] | None) -> tuple[list[dict], list[dict]]:
    """Converte i ``tool_calls`` (dialetto OpenAI) della risposta gateway nel
    formato interno: ``(tool_use_blocks, assistant_blocks)``.

    Ogni tool call OpenAI ``{id, function:{name, arguments(JSON string)}}``
    diventa un blocco ``{id, name, input(dict)}``. ``arguments`` viene
    deserializzato come JSON (fallback dict vuoto su parse fallito, come il
    punto unico ``_response_parsers.parse_openai_compatible_choice``).
    """
    tool_use_blocks: list[dict] = []
    assistant_blocks: list[dict] = []
    for tc in tool_calls or []:
        func = tc.get("function") or {}
        raw_args = func.get("arguments") or "{}"
        try:
            args = json.loads(raw_args) if isinstance(raw_args, str) else dict(raw_args)
        except Exception:  # noqa: BLE001
            args = {}
        block = {"id": tc.get("id", ""), "name": func.get("name", ""), "input": args}
        tool_use_blocks.append(block)
        assistant_blocks.append({"type": "tool_use", **block})
    return tool_use_blocks, assistant_blocks


def _usage_to_internal(usage: dict[str, Any] | None) -> dict[str, Any]:
    """Mappa LlmUsage del gateway nel dict usage interno (convenzione Anthropic
    input_tokens/output_tokens + chiavi cache lette da extract_usage_tokens)."""
    usage = usage or {}
    out: dict[str, Any] = {
        "input_tokens": int(usage.get("input_tokens") or 0),
        "output_tokens": int(usage.get("output_tokens") or 0),
    }
    cache_read = usage.get("cache_read_tokens")
    if cache_read:
        # chiave letta dal punto unico extract_usage_tokens (cache_read_tokens).
        out["cache_read_tokens"] = int(cache_read)
    cache_creation = usage.get("cache_creation_tokens")
    if cache_creation:
        out["cache_creation_tokens"] = int(cache_creation)
    return out


def _gateway_error_to_result(exc: Exception, provider: str, model: str) -> ProviderResult:
    """Mappa un errore HTTP/timeout della chiamata al gateway sul ProviderResult
    d'errore del brain.

    Per ``httpx.HTTPStatusError`` (gateway che risponde con status != 2xx) usa il
    punto unico ``error_handler.classify_error`` (l'eccezione httpx espone
    ``response.status_code``, gia' gestito da _extract_http_status_structured).
    Per timeout/connessione classifica direttamente. Regola F: nessun body
    grezzo nel messaggio loggato (solo status code).
    """
    from .error_handler import classify_error

    if isinstance(exc, httpx.TimeoutException):
        logger.error("gateway timeout per %s/%s", provider, model)
        meta = {
            "stop_reason": "error", "error": "Timeout della richiesta al gateway LLM.",
            "error_class": "timeout", "retriable": True, "backoff": True,
            "http_status": None, "retry_after_seconds": None,
        }
        return ProviderResult(
            provider=provider, model=model,
            content=f"[Error: {meta['error']}]", metadata=meta,
        )
    if isinstance(exc, httpx.HTTPStatusError):
        status = exc.response.status_code
        logger.error("gateway ha risposto %d per %s/%s", status, provider, model)
        info = classify_error(exc, provider)
        return ProviderResult(
            provider=provider, model=model,
            content=f"[Error: {info['message']}]",
            metadata={
                "stop_reason": "error",
                "error": info["message"],
                "error_class": info["stop_reason"],
                "retriable": info["retriable"],
                "backoff": info.get("backoff", False),
                "http_status": info.get("http_status"),
                "retry_after_seconds": info.get("retry_after_seconds"),
            },
        )
    # Connessione rifiutata / DNS / altri errori httpx.
    logger.error("gateway non raggiungibile per %s/%s: %s", provider, model, type(exc).__name__)
    info = classify_error(exc, provider)
    return ProviderResult(
        provider=provider, model=model,
        content=f"[Error: {info['message']}]",
        metadata={
            "stop_reason": "error",
            "error": info["message"],
            "error_class": info["stop_reason"],
            "retriable": info["retriable"],
            "backoff": info.get("backoff", False),
            "http_status": info.get("http_status"),
            "retry_after_seconds": info.get("retry_after_seconds"),
        },
    )


class GatewayProvider(BaseProvider):
    """Provider che delega al gateway LLM Rust via HTTP (httpx async).

    Implementa l'interfaccia ``BaseProvider`` (generate / generate_agent_turn /
    test_connection / list_models) ma non costruisce nessun client SDK vendor:
    inoltra al gateway che possiede routing/cooldown/privacy.
    """

    name = "gateway"

    def __init__(self) -> None:
        # Nessuno stato da inizializzare: URL/token sono letti on-demand
        # (DB-driven, regola G) cosi' un cambio di config non richiede restart.
        pass

    # ── Helper HTTP ───────────────────────────────────────────────────────────

    def _headers(self) -> dict[str, str]:
        return {
            "Authorization": f"Bearer {_service_token()}",
            "Content-Type": "application/json",
        }

    # ── generate (non agentico) ────────────────────────────────────────────────

    async def generate(self, model: str, prompt: str, **kwargs: Any) -> ProviderResult:
        """Completion semplice: un solo turno user, nessun tool. Inoltra a
        POST /v1/complete e mappa la LlmResponse nel ProviderResult."""
        pin_provider, concrete_model = _split_pin_provider(model)
        payload: dict[str, Any] = {
            "model": concrete_model,
            "messages": [{"role": "user", "content": prompt}],
            "metadata": _build_metadata("brain.generate"),
        }
        if pin_provider:
            payload["pin_provider"] = pin_provider
        if "temperature" in kwargs:
            payload["temperature"] = kwargs["temperature"]
        if "max_tokens" in kwargs:
            payload["max_tokens"] = kwargs["max_tokens"]
        try:
            data = await self._post_complete(payload)
        except Exception as exc:  # noqa: BLE001
            return _gateway_error_to_result(exc, self.name, model)

        usage = _usage_to_internal(data.get("usage"))
        return ProviderResult(
            provider=data.get("provider_used") or self.name,
            model=data.get("model_used") or model,
            content=data.get("content") or "",
            metadata={
                "usage": {
                    # generate() del brain usa la convenzione prompt/completion;
                    # il punto unico extract_usage_tokens accetta entrambe.
                    "prompt_tokens": usage["input_tokens"],
                    "completion_tokens": usage["output_tokens"],
                    "total_tokens": usage["input_tokens"] + usage["output_tokens"],
                },
                "finish_reason": data.get("finish_reason"),
            },
        )

    # ── generate_agent_turn (tool calling) ─────────────────────────────────────

    async def generate_agent_turn(
        self,
        model: str,
        messages: list[dict],
        tools: list[dict],
        max_tokens: int = 4096,
        system_text: str = "",
        force_tool_choice: bool | None = None,  # noqa: ARG002 (gateway decide il tool_choice)
        internal_task: bool = False,  # noqa: ARG002 (gateway gestisce il thinking)
        thinking: dict[str, Any] | None = None,
    ) -> ProviderResult:
        """Turno agente con tool calling, delegato a POST /v1/complete.

        Mappa messaggi+tool nel dialetto del gateway e ri-normalizza la
        LlmResponse al formato Anthropic interno (stop_reason, tool_use_blocks,
        assistant_content con i blocchi thinking/text/tool_use, usage).

        ``force_tool_choice`` / ``internal_task`` sono accettati per compatibilita'
        con la firma chiamata dal registry (introspezione difensiva), ma il
        tool_choice e la policy thinking sono decisi dal gateway/provider a valle:
        non li ri-deriviamo qui per non duplicare logica (regola L).
        """
        pin_provider, concrete_model = _split_pin_provider(model)
        payload: dict[str, Any] = {
            "model": concrete_model,
            "messages": _to_gateway_messages(messages, system_text),
            "max_tokens": max_tokens,
            "metadata": _build_metadata("brain.agent_turn"),
        }
        if pin_provider:
            payload["pin_provider"] = pin_provider
        gw_tools = _to_gateway_tools(tools) if tools else []
        if gw_tools:
            payload["tools"] = gw_tools
        if thinking and thinking.get("enabled"):
            tcfg: dict[str, Any] = {"enabled": True}
            if thinking.get("budget_tokens"):
                tcfg["budget_tokens"] = int(thinking["budget_tokens"])
            payload["thinking"] = tcfg

        try:
            data = await self._post_complete(payload)
        except Exception as exc:  # noqa: BLE001
            return _gateway_error_to_result(exc, self.name, model)

        return self._build_agent_result(data, model)

    def _build_agent_result(self, data: dict[str, Any], model: str) -> ProviderResult:
        """Costruisce il ProviderResult del turno agentico dalla LlmResponse.

        - tool_calls -> tool_use_blocks + blocchi assistant tool_use;
        - reasoning -> blocco assistant ``{"type": "thinking", ...}`` con la
          ``thinking_signature`` per il round-trip (l'ordine mette il thinking
          prima del testo/tool, come l'adapter Anthropic);
        - text -> blocco assistant ``{"type": "text", ...}``.
        stop_reason: ``tool_use`` se ci sono tool call, altrimenti ``end_turn``.
        """
        content = data.get("content") or ""
        reasoning = data.get("reasoning") or ""
        signature = data.get("thinking_signature") or ""
        tool_use_blocks, tool_assistant_blocks = _tool_calls_to_blocks(data.get("tool_calls"))

        assistant_content: list[dict] = []
        if reasoning:
            thinking_block: dict[str, Any] = {"type": "thinking", "thinking": reasoning}
            if signature:
                thinking_block["signature"] = signature
            assistant_content.append(thinking_block)
        if content:
            assistant_content.append({"type": "text", "text": content})
        assistant_content.extend(tool_assistant_blocks)

        stop_reason = "tool_use" if tool_use_blocks else "end_turn"

        return ProviderResult(
            provider=data.get("provider_used") or self.name,
            model=data.get("model_used") or model,
            content=content,
            metadata={
                "stop_reason": stop_reason,
                "tool_use_blocks": tool_use_blocks,
                "assistant_content": assistant_content,
                "usage": _usage_to_internal(data.get("usage")),
                # round-trip: esposta anche a livello metadata per i chiamanti che
                # la propagano senza ispezionare assistant_content.
                **({"thinking_signature": signature} if signature else {}),
            },
        )

    async def _post_complete(self, payload: dict[str, Any]) -> dict[str, Any]:
        """POST /v1/complete. Solleva su status != 2xx (raise_for_status) e su
        timeout/connessione. Il chiamante mappa l'eccezione con
        ``_gateway_error_to_result``."""
        url = f"{_gateway_url()}/v1/complete"
        async with httpx.AsyncClient(timeout=_complete_timeout_s()) as client:
            resp = await client.post(url, json=payload, headers=self._headers())
            resp.raise_for_status()
            return resp.json()

    # ── stream (SSE) ────────────────────────────────────────────────────────────

    async def stream(
        self,
        model: str,
        messages: list[dict],
        tools: list[dict] | None = None,
        max_tokens: int = 4096,
        system_text: str = "",
        thinking: dict[str, Any] | None = None,
    ) -> AsyncIterator[dict[str, Any]]:
        """Streaming via POST /v1/stream: consuma l'SSE (`data: {json}` fino a
        `data: [DONE]`) e yield i chunk normalizzati nel formato interno.

        Ogni chunk yieldato e' un dict:
          {
            "delta": str,                  # testo incrementale
            "reasoning_delta": str | None, # thinking incrementale
            "tool_call_delta": dict | None,# delta tool call (dialetto OpenAI)
            "finish_reason": str | None,
            "usage": dict | None,          # usage interno (input/output_tokens)
          }
        Il consumatore aggrega i delta. Errori di rete -> yield di un chunk con
        ``finish_reason="error"`` + ``error``/``error_class`` (nessun raise per
        non rompere il generatore lato chiamante).
        """
        pin_provider, concrete_model = _split_pin_provider(model)
        payload: dict[str, Any] = {
            "model": concrete_model,
            "messages": _to_gateway_messages(messages, system_text),
            "max_tokens": max_tokens,
            "stream": True,
            "metadata": _build_metadata("brain.stream"),
        }
        if pin_provider:
            payload["pin_provider"] = pin_provider
        gw_tools = _to_gateway_tools(tools) if tools else []
        if gw_tools:
            payload["tools"] = gw_tools
        if thinking and thinking.get("enabled"):
            tcfg: dict[str, Any] = {"enabled": True}
            if thinking.get("budget_tokens"):
                tcfg["budget_tokens"] = int(thinking["budget_tokens"])
            payload["thinking"] = tcfg

        url = f"{_gateway_url()}/v1/stream"
        try:
            async with httpx.AsyncClient(timeout=_stream_timeout_s()) as client:
                async with client.stream(
                    "POST", url, json=payload, headers=self._headers()
                ) as resp:
                    resp.raise_for_status()
                    async for chunk in self._iter_sse_chunks(resp):
                        yield chunk
        except httpx.HTTPStatusError as exc:
            status = exc.response.status_code
            logger.error("gateway stream ha risposto %d per %s/%s", status, self.name, model)
            yield {
                "delta": "", "reasoning_delta": None, "tool_call_delta": None,
                "finish_reason": "error",
                "error": f"Gateway stream HTTP {status}",
                "error_class": "provider_error" if status >= 500 else "invalid_request",
            }
        except Exception as exc:  # noqa: BLE001
            logger.error("gateway stream fallito per %s/%s: %s", self.name, model, type(exc).__name__)
            yield {
                "delta": "", "reasoning_delta": None, "tool_call_delta": None,
                "finish_reason": "error",
                "error": "Errore di connessione al gateway LLM (stream).",
                "error_class": "connection_error",
            }

    @staticmethod
    async def _iter_sse_chunks(resp: httpx.Response) -> AsyncIterator[dict[str, Any]]:
        """Parser SSE: linee ``data: {json}`` terminate da ``data: [DONE]``.

        Statico e separato per essere testabile con una risposta httpx mockata.
        Ignora linee vuote e linee non-``data:`` (commenti SSE/heartbeat). Una
        linea ``data:`` con JSON non valido viene saltata (log debug, nessun
        crash dello stream).
        """
        async for raw_line in resp.aiter_lines():
            line = raw_line.strip()
            if not line or not line.startswith("data:"):
                continue
            data_str = line[len("data:"):].strip()
            if data_str == "[DONE]":
                break
            try:
                obj = json.loads(data_str)
            except Exception:  # noqa: BLE001
                logger.debug("gateway stream: linea data non-JSON ignorata (len=%d)", len(data_str))
                continue
            yield {
                "delta": obj.get("delta", ""),
                "reasoning_delta": obj.get("reasoning_delta"),
                "tool_call_delta": obj.get("tool_call_delta"),
                "finish_reason": obj.get("finish_reason"),
                "usage": _usage_to_internal(obj["usage"]) if obj.get("usage") else None,
            }

    # ── test_connection / list_models ───────────────────────────────────────────

    async def test_connection(self) -> dict[str, Any]:
        """Verifica la raggiungibilita' del gateway via GET /health (endpoint del
        gateway Rust). Non chiama un provider reale (nessun costo)."""
        url = f"{_gateway_url()}/health"
        try:
            async with httpx.AsyncClient(timeout=5.0) as client:
                resp = await client.get(url, headers=self._headers())
            if resp.status_code == 200:
                return {"provider": self.name, "status": "ready"}
            return {
                "provider": self.name, "status": "error",
                "reason": f"gateway health HTTP {resp.status_code}",
                "error_class": "provider_error",
            }
        except Exception as exc:  # noqa: BLE001
            from .error_handler import classify_error
            info = classify_error(exc, self.name)
            return {
                "provider": self.name, "status": "error",
                "reason": info["message"], "error_class": info["stop_reason"],
            }

    def list_models(self) -> list[ProviderCatalogEntry]:
        """Il gateway non e' un "vendor" con un proprio catalogo: i modelli
        restano governati dalle tabelle DB (ai_price_catalog / routing matrix).
        Ritorna lista vuota — il GatewayProvider non espone un catalogo proprio."""
        return []
