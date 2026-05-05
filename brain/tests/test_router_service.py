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


def test_classify_file_ops_via_keywords() -> None:
    router = SemanticRouter()
    out = router._classify_by_keywords("Per favore elimina i file Dockerfile rimasti nel progetto")
    assert out["intent"] == "file_ops", out


def test_classify_system_admin_via_keywords() -> None:
    router = SemanticRouter()
    out = router._classify_by_keywords("Esegui docker compose down per fermare i container")
    assert out["intent"] == "system_admin", out


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
    mock_singleton.return_value.decide.assert_called_once_with(
        message="elimina i file", behavior_mode="bilanciata",
    )


def test_route_model_fallback_when_rust_unreachable() -> None:
    """Se il client HTTP fallisce, route_model deve restituire un fallback safe
    (openai/gpt-4.1-mini), mai sollevare eccezione."""
    from brain.router.service import _RoutingClient
    client = _RoutingClient(base_url="http://127.0.0.1:1")  # porta sicuramente chiusa
    decision = client.decide(message="anything", behavior_mode="bilanciata")
    assert decision.provider == "openai"
    assert decision.model == "gpt-4.1-mini"
    assert decision.confidence < 0.9, "fallback deve avere confidence ridotta"


def test_routing_client_caches_per_message() -> None:
    """Cache LRU-like: chiamate ripetute con stesso (msg, mode) entro 30s
    non rifanno HTTP."""
    from brain.router.service import _RoutingClient
    fake = RoutingDecision(provider="anthropic", model="claude-haiku-4-5-20251001", rationale="m", confidence=0.92)
    client = _RoutingClient(base_url="http://test")
    # Inietto a mano nella cache un'entry valida
    import time
    client._cache[("hello", "bilanciata")] = (time.monotonic(), fake)
    # La seconda chiamata deve restituire dalla cache (no HTTP -> no eccezione)
    d = client.decide(message="hello", behavior_mode="bilanciata")
    assert d.provider == "anthropic"
    assert d.model == "claude-haiku-4-5-20251001"
