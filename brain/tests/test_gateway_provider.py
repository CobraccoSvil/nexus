"""Test del GatewayProvider (brain/providers/gateway_provider.py) SENZA rete.

Verifica le conversioni del contratto col gateway LLM Rust mockando httpx:
  - conversione richiesta interna del brain -> payload LlmRequest del gateway
    (messaggi Anthropic-like -> dialetto OpenAI, tool, metadata, thinking);
  - parsing LlmResponse del gateway -> ProviderResult interno (content,
    tool_use_blocks, assistant_content, usage);
  - parsing di un chunk SSE dello stream;
  - round-trip della thinking_signature (request e response).

Idempotenti, auto-contenuti: nessun DB, nessun gateway reale. La rete e' tagliata
sostituendo httpx.AsyncClient con un doppio che cattura il payload e restituisce
una risposta predefinita.
"""
from __future__ import annotations

import json
from typing import Any

import pytest

from brain.providers import gateway_provider as gp
from brain.providers.base import ProviderResult


# ──────────────────────────────────────────────────────────────────────────────
# Doppi di test per httpx (nessuna rete)
# ──────────────────────────────────────────────────────────────────────────────


class _FakeResponse:
    """Risposta httpx fittizia per /v1/complete."""

    def __init__(self, payload: dict[str, Any], status_code: int = 200) -> None:
        self._payload = payload
        self.status_code = status_code

    def raise_for_status(self) -> None:
        if self.status_code >= 400:  # pragma: no cover - non usato nei test felici
            raise AssertionError("status >= 400")

    def json(self) -> dict[str, Any]:
        return self._payload


class _CapturingClient:
    """AsyncClient fittizio che cattura il payload POST e ritorna una risposta
    predefinita. Usato come ``httpx.AsyncClient(...)`` context manager async."""

    captured_payload: dict[str, Any] | None = None
    captured_headers: dict[str, str] | None = None

    def __init__(self, response_payload: dict[str, Any]) -> None:
        self._response_payload = response_payload

    def __call__(self, *args: Any, **kwargs: Any) -> "_CapturingClient":
        # Permette l'uso come factory: gp.httpx.AsyncClient = _CapturingClient(...)
        return self

    async def __aenter__(self) -> "_CapturingClient":
        return self

    async def __aexit__(self, *exc: Any) -> None:
        return None

    async def post(self, url: str, json: dict[str, Any], headers: dict[str, str]) -> _FakeResponse:  # noqa: A002
        type(self).captured_payload = json
        type(self).captured_headers = headers
        return _FakeResponse(self._response_payload)


class _FakeStreamResponse:
    """Risposta httpx fittizia per /v1/stream: espone aiter_lines()."""

    def __init__(self, lines: list[str], status_code: int = 200) -> None:
        self._lines = lines
        self.status_code = status_code

    def raise_for_status(self) -> None:
        if self.status_code >= 400:  # pragma: no cover
            raise AssertionError("status >= 400")

    async def aiter_lines(self):
        for line in self._lines:
            yield line


def _install_complete_double(
    monkeypatch: pytest.MonkeyPatch, response_payload: dict[str, Any]
) -> type[_CapturingClient]:
    """Sostituisce httpx.AsyncClient con il doppio che cattura il payload."""
    double = _CapturingClient(response_payload)
    monkeypatch.setattr(gp.httpx, "AsyncClient", double)
    return type(double)


# ──────────────────────────────────────────────────────────────────────────────
# URL e token (regola G)
# ──────────────────────────────────────────────────────────────────────────────


def test_url_da_env_override(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("NEXUS_GATEWAY_URL", "http://gw.local:9999/")
    assert gp._gateway_url() == "http://gw.local:9999"


def test_token_da_env_con_fallback_dev(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("NEXUS_GATEWAY_SERVICE_TOKEN", raising=False)
    assert gp._service_token() == gp._DEV_SERVICE_TOKEN
    monkeypatch.setenv("NEXUS_GATEWAY_SERVICE_TOKEN", "prod-token-123")
    assert gp._service_token() == "prod-token-123"


def test_headers_includono_bearer(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("NEXUS_GATEWAY_SERVICE_TOKEN", "abc")
    headers = gp.GatewayProvider()._headers()
    assert headers["Authorization"] == "Bearer abc"
    assert headers["Content-Type"] == "application/json"


# ──────────────────────────────────────────────────────────────────────────────
# Conversione richiesta brain -> LlmRequest del gateway
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_generate_agent_turn_costruisce_request_gateway(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """I messaggi Anthropic-like (con tool_use/tool_result) e i tool diventano il
    dialetto OpenAI del gateway; system_text in testa; metadata presenti."""
    captured = _install_complete_double(
        monkeypatch,
        {
            "content": "ok",
            "usage": {"input_tokens": 10, "output_tokens": 5},
            "model_used": "claude-x",
            "provider_used": "anthropic",
            "finish_reason": "end_turn",
        },
    )
    messages = [
        {"role": "user", "content": "leggi il file"},
        {
            "role": "assistant",
            "content": [
                {"type": "text", "text": "ora leggo"},
                {"type": "tool_use", "id": "call_1", "name": "read", "input": {"path": "/a"}},
            ],
        },
        {
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "call_1", "content": "contenuto"},
            ],
        },
    ]
    tools = [
        {"name": "read", "description": "Legge un file", "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}}},
    ]

    await gp.GatewayProvider().generate_agent_turn(
        "claude-x", messages, tools, max_tokens=2048, system_text="sei un agente",
    )

    payload = captured.captured_payload
    assert payload is not None
    assert payload["model"] == "claude-x"
    # model senza prefisso provider noto -> nessun pin (routing storico gateway).
    assert "pin_provider" not in payload
    assert payload["max_tokens"] == 2048
    # system_text -> primo messaggio role=system
    assert payload["messages"][0] == {"role": "system", "content": "sei un agente"}
    # user iniziale
    assert payload["messages"][1]["role"] == "user"
    # assistant con tool_calls (dialetto OpenAI)
    asst = payload["messages"][2]
    assert asst["role"] == "assistant"
    assert asst["tool_calls"][0]["function"]["name"] == "read"
    assert json.loads(asst["tool_calls"][0]["function"]["arguments"]) == {"path": "/a"}
    # tool_result -> messaggio role=tool con tool_call_id
    tool_msg = payload["messages"][3]
    assert tool_msg["role"] == "tool"
    assert tool_msg["tool_call_id"] == "call_1"
    # tool definition nel dialetto del gateway
    assert payload["tools"][0]["type"] == "function"
    assert payload["tools"][0]["function"]["name"] == "read"
    # metadata richiesto dal contratto
    assert payload["metadata"]["feature"] == "brain.agent_turn"
    assert "request_id" in payload["metadata"]


@pytest.mark.asyncio
async def test_generate_request_minimale(monkeypatch: pytest.MonkeyPatch) -> None:
    captured = _install_complete_double(
        monkeypatch,
        {
            "content": "ciao",
            "usage": {"input_tokens": 3, "output_tokens": 2},
            "model_used": "m",
            "provider_used": "p",
            "finish_reason": "stop",
        },
    )
    await gp.GatewayProvider().generate("m", "dimmi ciao", temperature=0.2, max_tokens=64)
    payload = captured.captured_payload
    assert payload["messages"] == [{"role": "user", "content": "dimmi ciao"}]
    assert payload["temperature"] == 0.2
    assert payload["max_tokens"] == 64
    # model "m" senza prefisso provider -> nessun pin.
    assert "pin_provider" not in payload
    assert payload["metadata"]["feature"] == "brain.generate"


# ──────────────────────────────────────────────────────────────────────────────
# Split provider/model -> pin_provider (esecuzione ESATTA lato gateway)
# ──────────────────────────────────────────────────────────────────────────────


def test_split_pin_provider_helper() -> None:
    """``_split_pin_provider`` separa il provider noto dal model concreto.

    - prefisso provider noto -> (provider, model_senza_prefisso);
    - nessun ``/`` -> (None, model invariato);
    - prefisso NON noto -> (None, model invariato);
    - piu' segmenti -> split SOLO sul primo ``/`` (resto come model concreto);
    - prefisso case-insensitive normalizzato a minuscolo.
    """
    assert gp._split_pin_provider("anthropic/claude-x") == ("anthropic", "claude-x")
    assert gp._split_pin_provider("openai/gpt-4o-mini") == ("openai", "gpt-4o-mini")
    assert gp._split_pin_provider("gpt-4o-mini") == (None, "gpt-4o-mini")
    # "claude-x" contiene un prefisso non-provider con "/": resta invariato.
    assert gp._split_pin_provider("some-vendor/model") == (None, "some-vendor/model")
    # split solo sul primo "/": il resto (anche con "/") e' il model concreto.
    assert gp._split_pin_provider("vllm/org/modello") == ("vllm", "org/modello")
    # case-insensitive sul prefisso.
    assert gp._split_pin_provider("Anthropic/claude-x") == ("anthropic", "claude-x")
    # componente vuota dopo "/" -> nessun pin.
    assert gp._split_pin_provider("openai/") == (None, "openai/")


@pytest.mark.asyncio
async def test_agent_turn_model_prefissato_setta_pin_provider(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Un model ``"<provider>/<model>"`` viene splittato: il payload porta
    ``pin_provider=<provider>`` e ``model=<model concreto senza prefisso>``,
    cosi' il gateway esegue ESATTAMENTE quel provider (no routing, no fallback
    cross-provider)."""
    captured = _install_complete_double(
        monkeypatch,
        {
            "content": "ok",
            "usage": {"input_tokens": 10, "output_tokens": 5},
            "model_used": "claude-x",
            "provider_used": "anthropic",
            "finish_reason": "end_turn",
        },
    )
    await gp.GatewayProvider().generate_agent_turn(
        "anthropic/claude-x", [{"role": "user", "content": "ciao"}], [],
    )
    payload = captured.captured_payload
    assert payload["pin_provider"] == "anthropic"
    assert payload["model"] == "claude-x"


@pytest.mark.asyncio
async def test_agent_turn_model_senza_prefisso_non_setta_pin_provider(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Un model senza prefisso provider noto NON attiva il pin: il payload non
    contiene ``pin_provider`` e il model resta invariato (routing storico)."""
    captured = _install_complete_double(
        monkeypatch,
        {
            "content": "ok",
            "usage": {"input_tokens": 1, "output_tokens": 1},
            "model_used": "gpt-4o-mini",
            "provider_used": "openai",
            "finish_reason": "end_turn",
        },
    )
    await gp.GatewayProvider().generate_agent_turn(
        "gpt-4o-mini", [{"role": "user", "content": "ciao"}], [],
    )
    payload = captured.captured_payload
    assert "pin_provider" not in payload
    assert payload["model"] == "gpt-4o-mini"


@pytest.mark.asyncio
async def test_generate_model_prefissato_setta_pin_provider(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Anche la completion semplice ``generate`` splitta il prefisso provider."""
    captured = _install_complete_double(
        monkeypatch,
        {
            "content": "ciao",
            "usage": {"input_tokens": 1, "output_tokens": 1},
            "model_used": "gpt-4o-mini",
            "provider_used": "openai",
            "finish_reason": "stop",
        },
    )
    await gp.GatewayProvider().generate("openai/gpt-4o-mini", "ping")
    payload = captured.captured_payload
    assert payload["pin_provider"] == "openai"
    assert payload["model"] == "gpt-4o-mini"


# ──────────────────────────────────────────────────────────────────────────────
# Parsing LlmResponse -> ProviderResult interno
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_parsing_risposta_con_tool_calls(monkeypatch: pytest.MonkeyPatch) -> None:
    _install_complete_double(
        monkeypatch,
        {
            "content": "",
            "tool_calls": [
                {"id": "tc1", "type": "function", "function": {"name": "write", "arguments": "{\"path\": \"/b\", \"text\": \"x\"}"}},
            ],
            "usage": {"input_tokens": 100, "output_tokens": 20, "cache_read_tokens": 30},
            "model_used": "claude-x",
            "provider_used": "anthropic",
            "finish_reason": "tool_calls",
        },
    )
    res = await gp.GatewayProvider().generate_agent_turn("claude-x", [{"role": "user", "content": "scrivi"}], [])
    assert isinstance(res, ProviderResult)
    assert res.metadata["stop_reason"] == "tool_use"
    blocks = res.metadata["tool_use_blocks"]
    assert blocks == [{"id": "tc1", "name": "write", "input": {"path": "/b", "text": "x"}}]
    # assistant_content rispecchia il blocco tool_use
    assert {"type": "tool_use", "id": "tc1", "name": "write", "input": {"path": "/b", "text": "x"}} in res.metadata["assistant_content"]
    # usage normalizzato (convenzione Anthropic input/output + cache)
    assert res.metadata["usage"]["input_tokens"] == 100
    assert res.metadata["usage"]["output_tokens"] == 20
    assert res.metadata["usage"]["cache_read_tokens"] == 30
    # provider/model dalla risposta del gateway
    assert res.provider == "anthropic"
    assert res.model == "claude-x"


@pytest.mark.asyncio
async def test_parsing_risposta_testuale(monkeypatch: pytest.MonkeyPatch) -> None:
    _install_complete_double(
        monkeypatch,
        {
            "content": "risposta finale",
            "usage": {"input_tokens": 8, "output_tokens": 4},
            "model_used": "m",
            "provider_used": "p",
            "finish_reason": "end_turn",
        },
    )
    res = await gp.GatewayProvider().generate_agent_turn("m", [{"role": "user", "content": "ciao"}], [])
    assert res.content == "risposta finale"
    assert res.metadata["stop_reason"] == "end_turn"
    assert res.metadata["tool_use_blocks"] == []
    assert res.metadata["assistant_content"] == [{"type": "text", "text": "risposta finale"}]


# ──────────────────────────────────────────────────────────────────────────────
# Round-trip thinking_signature
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_thinking_signature_round_trip(monkeypatch: pytest.MonkeyPatch) -> None:
    """La signature ritorna nella risposta (metadata + blocco thinking) e, se
    rispedita in un turno successivo, viene attaccata al messaggio assistant del
    gateway come campo thinking_signature."""
    # 1) Risposta del gateway che include reasoning + signature.
    captured = _install_complete_double(
        monkeypatch,
        {
            "content": "deciso",
            "reasoning": "ragiono...",
            "thinking_signature": "SIG-OPAQUE-123",
            "usage": {"input_tokens": 5, "output_tokens": 3},
            "model_used": "claude-x",
            "provider_used": "anthropic",
            "finish_reason": "end_turn",
        },
    )
    res = await gp.GatewayProvider().generate_agent_turn("claude-x", [{"role": "user", "content": "pensa"}], [])
    # signature esposta a livello metadata
    assert res.metadata["thinking_signature"] == "SIG-OPAQUE-123"
    # e dentro il blocco thinking dell'assistant_content (per il round-trip)
    thinking_block = res.metadata["assistant_content"][0]
    assert thinking_block["type"] == "thinking"
    assert thinking_block["thinking"] == "ragiono..."
    assert thinking_block["signature"] == "SIG-OPAQUE-123"

    # 2) Rispedisco quel turno assistant (con il blocco thinking) in un nuovo turno:
    #    la signature deve finire come campo thinking_signature del messaggio gateway.
    followup_messages = [
        {"role": "user", "content": "pensa"},
        {"role": "assistant", "content": res.metadata["assistant_content"]},
        {"role": "user", "content": "continua"},
    ]
    await gp.GatewayProvider().generate_agent_turn("claude-x", followup_messages, [])
    payload = captured.captured_payload
    # trova il messaggio assistant nel payload inviato
    asst = next(m for m in payload["messages"] if m["role"] == "assistant")
    assert asst["thinking_signature"] == "SIG-OPAQUE-123"


# ──────────────────────────────────────────────────────────────────────────────
# Parsing chunk SSE (stream)
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_iter_sse_chunks_parsing() -> None:
    """Il parser SSE estrae i delta, ignora linee vuote/non-data e si ferma a [DONE]."""
    lines = [
        ": heartbeat",  # commento SSE -> ignorato
        "",  # vuota -> ignorata
        'data: {"delta": "Ciao", "finish_reason": null}',
        'data: {"delta": " mondo", "reasoning_delta": "rag"}',
        'data: {"delta": "", "finish_reason": "stop", "usage": {"input_tokens": 7, "output_tokens": 9}}',
        "data: [DONE]",
        'data: {"delta": "DOPO-DONE"}',  # non deve essere emesso
    ]
    resp = _FakeStreamResponse(lines)
    chunks = [c async for c in gp.GatewayProvider._iter_sse_chunks(resp)]

    assert len(chunks) == 3
    assert chunks[0]["delta"] == "Ciao"
    assert chunks[1]["delta"] == " mondo"
    assert chunks[1]["reasoning_delta"] == "rag"
    assert chunks[2]["finish_reason"] == "stop"
    # usage del chunk finale normalizzato al formato interno
    assert chunks[2]["usage"]["input_tokens"] == 7
    assert chunks[2]["usage"]["output_tokens"] == 9


@pytest.mark.asyncio
async def test_iter_sse_chunks_salta_json_invalido() -> None:
    lines = [
        'data: {"delta": "ok"}',
        "data: {non-json",  # JSON invalido -> saltato senza crash
        "data: [DONE]",
    ]
    resp = _FakeStreamResponse(lines)
    chunks = [c async for c in gp.GatewayProvider._iter_sse_chunks(resp)]
    assert len(chunks) == 1
    assert chunks[0]["delta"] == "ok"


# ──────────────────────────────────────────────────────────────────────────────
# Gestione errori (mappatura sul formato errore del brain)
# ──────────────────────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_errore_timeout_mappa_su_provider_result(monkeypatch: pytest.MonkeyPatch) -> None:
    import httpx

    class _TimeoutClient:
        def __call__(self, *a: Any, **k: Any) -> "_TimeoutClient":
            return self

        async def __aenter__(self) -> "_TimeoutClient":
            return self

        async def __aexit__(self, *exc: Any) -> None:
            return None

        async def post(self, *a: Any, **k: Any):
            raise httpx.TimeoutException("timed out")

    monkeypatch.setattr(gp.httpx, "AsyncClient", _TimeoutClient())
    res = await gp.GatewayProvider().generate_agent_turn("m", [{"role": "user", "content": "x"}], [])
    assert res.content.startswith("[Error:")
    assert res.metadata["stop_reason"] == "error"
    assert res.metadata["error_class"] == "timeout"
    assert res.metadata["retriable"] is True


def test_list_models_vuoto() -> None:
    # Il gateway non espone un catalogo proprio (governato dal DB).
    assert gp.GatewayProvider().list_models() == []


# ──────────────────────────────────────────────────────────────────────────────
# Vision: blocco image_url propagato intatto al gateway
# ──────────────────────────────────────────────────────────────────────────────


def test_convert_messages_propaga_image_url() -> None:
    """convert_messages_to_openai con un blocco image_url emette un content-ARRAY
    (parti text + image_url) invece della stringa. Senza immagini resta stringa."""
    from brain.providers.adapter_base import convert_messages_to_openai

    # Con immagine: content e' una lista di parti, l'image_url passa intatto.
    out = convert_messages_to_openai(
        [
            {
                "role": "user",
                "content": [
                    {"type": "text", "text": "descrivi"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
                ],
            }
        ]
    )
    assert len(out) == 1
    content = out[0]["content"]
    assert isinstance(content, list)
    assert content[0] == {"type": "text", "text": "descrivi"}
    assert content[1] == {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}

    # Senza immagine: comportamento storico (content stringa).
    out_text = convert_messages_to_openai(
        [{"role": "user", "content": [{"type": "text", "text": "solo testo"}]}]
    )
    assert out_text[0]["content"] == "solo testo"


@pytest.mark.asyncio
async def test_generate_agent_turn_payload_con_immagine(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Il blocco image_url di un messaggio user arriva nel payload del gateway
    come content-array (vision via gateway, non SDK diretto)."""
    captured = _install_complete_double(
        monkeypatch,
        {
            "content": "DESCRIZIONE: un gatto\nOCR:",
            "usage": {"input_tokens": 100, "output_tokens": 8},
            "model_used": "gemini-x",
            "provider_used": "google",
            "finish_reason": "end_turn",
        },
    )
    messages = [
        {
            "role": "user",
            "content": [
                {"type": "text", "text": "descrivi"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,ZZZ"}},
            ],
        }
    ]
    # model "provider/model" -> pin del provider sul gateway.
    res = await gp.GatewayProvider().generate_agent_turn(
        "google/gemini-x", messages, [], max_tokens=2048,
    )
    assert res.content == "DESCRIZIONE: un gatto\nOCR:"

    payload = captured.captured_payload
    assert payload is not None
    assert payload["model"] == "gemini-x"
    assert payload["pin_provider"] == "google"
    user_msg = payload["messages"][0]
    assert user_msg["role"] == "user"
    assert isinstance(user_msg["content"], list)
    assert user_msg["content"][1]["type"] == "image_url"
    assert user_msg["content"][1]["image_url"]["url"] == "data:image/png;base64,ZZZ"
