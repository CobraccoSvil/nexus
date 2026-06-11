"""Unit test per il SemanticRouter.

Dopo la Fase A del consolidamento (vedi piano `questo-lo-stesso-proud-blossom.md`),
la matrice di routing e la detection task rischiosi NON sono piu' in Python:
risiedono in `crates/mcp-core/src/orchestrator.rs`. Il router Python e' un
thin client che delega via HTTP all'endpoint `/api/internal/routing/decide`.

Quindi i test qui:
- verificano la classificazione locale degli intent (rimasta in Python come
  complemento embedding-based + keyword fallback);
- verificano che `route_model()` chiami il client HTTP e ne propaghi la
  risposta (con fallback safe se l'endpoint non risponde).

I test sulla matrice/risky vivono ora lato Rust in
`crates/mcp-core/src/orchestrator.rs::tests::*` (e nei test #[cfg(test)] in
`internal_routing.rs`).
"""
from __future__ import annotations

from unittest.mock import patch

from brain.router.service import RoutingDecision, SemanticRouter


def test_route_model_delegates_to_rust_endpoint() -> None:
    """Verifica che route_model chiami il thin client e ritorni la decisione Rust."""
    router = SemanticRouter()
    fake = RoutingDecision(
        provider="anthropic",
        model="claude-sonnet-4-6",
        rationale="mocked",
        confidence=0.9,
    )
    with patch("brain.router.service._routing_client_singleton") as mock_singleton:
        mock_singleton.return_value.decide.return_value = fake
        decision = router.route_model("file_ops", 800, "bilanciata", message="elimina i file")
    assert decision.provider == "anthropic"
    assert decision.model == "claude-sonnet-4-6"
    # L'intent gia' classificato dal brain viene propagato al /decide Rust
    # (commit 4f1c99d): mcp-core salta la classificazione LLM ridondante.
    mock_singleton.return_value.decide.assert_called_once_with(
        message="elimina i file", behavior_mode="bilanciata", intent="file_ops",
    )


def test_route_model_fallback_when_rust_unreachable() -> None:
    """Se il client HTTP fallisce, `decide` NON inventa un modello (regola G:
    niente magic fallback): ritorna la sentinella __router_unavailable__ con
    confidence 0, mai sollevare eccezione. Il chiamante a monte intercetta la
    sentinella e ferma il flusso invece di usare un modello arbitrario."""
    from brain.router.service import _RoutingClient
    client = _RoutingClient(base_url="http://127.0.0.1:1")  # porta sicuramente chiusa
    decision = client.decide(message="anything", behavior_mode="bilanciata")
    assert decision.provider == "__router_unavailable__"
    assert decision.model == "__router_unavailable__"
    assert decision.confidence == 0.0


def test_purpose_model_503_returns_no_capable() -> None:
    """ADR 0020: se il gate ritorna 503 (purpose su provider in cooldown senza
    alternativa), il client deve propagare __no_capable_provider__ — non
    __router_unavailable__ — cosi' il chiamante salta il fallback morto."""
    import io
    import urllib.error
    from brain.router.service import _RoutingClient

    def _raise_503(req, timeout=None):  # noqa: ARG001
        raise urllib.error.HTTPError(
            url="http://test/api/internal/routing/purpose",
            code=503,
            msg="Service Unavailable",
            hdrs=None,
            fp=io.BytesIO(b'{"rationale":"purpose_model:in_cooldown","no_capable_provider":true}'),
        )

    client = _RoutingClient(base_url="http://test")
    with patch("urllib.request.urlopen", side_effect=_raise_503):
        d = client.purpose_model(purpose="loop_fallback_default")
    assert d.provider == "__no_capable_provider__"
    assert d.model == "__no_capable_provider__"
    assert d.confidence == 0.0


def test_cooldown_providers_parses_lowercase_set() -> None:
    """cooldown_providers() ritorna l'insieme dei provider in cooldown dal gate."""
    import io
    from brain.router.service import _RoutingClient

    class _Resp:
        def __init__(self, payload: bytes) -> None:
            self._b = io.BytesIO(payload)

        def __enter__(self):  # noqa: ANN001
            return self

        def __exit__(self, *a):  # noqa: ANN001
            return False

        def read(self) -> bytes:
            return self._b.read()

    payload = b'{"providers":[{"provider":"Anthropic","seconds_remaining":60},{"provider":"openai","seconds_remaining":30}]}'
    client = _RoutingClient(base_url="http://test")
    with patch("urllib.request.urlopen", return_value=_Resp(payload)):
        out = client.cooldown_providers()
    assert out == {"anthropic", "openai"}


def test_cooldown_providers_none_when_unreachable() -> None:
    """Se il gate non risponde, cooldown_providers() ritorna None (il caller usa
    la vista locale come degrado, non come fonte primaria)."""
    from brain.router.service import _RoutingClient

    client = _RoutingClient(base_url="http://127.0.0.1:1")  # porta chiusa
    assert client.cooldown_providers() is None


def test_routing_client_caches_per_message() -> None:
    """Cache LRU-like: chiamate ripetute con stesso (msg, mode, intent) entro
    30s non rifanno HTTP. La chiave e' una tripla con intent (o "" se assente)
    da quando il /decide riceve l'intent gia' classificato (commit 4f1c99d)."""
    from brain.router.service import _RoutingClient
    fake = RoutingDecision(provider="anthropic", model="claude-haiku-4-5-20251001", rationale="m", confidence=0.92)
    client = _RoutingClient(base_url="http://test")
    # Inietto a mano nella cache un'entry valida
    import time
    client._cache[("hello", "bilanciata", "")] = (time.monotonic(), fake)
    # La seconda chiamata deve restituire dalla cache (no HTTP -> no eccezione)
    d = client.decide(message="hello", behavior_mode="bilanciata")
    assert d.provider == "anthropic"
    assert d.model == "claude-haiku-4-5-20251001"
