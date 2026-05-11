"""Test L4+: chain di provider per il classifier (mig 0134).

Verifica che il classifier itera la chain di provider e cade sul prossimo
quando uno fallisce (timeout, exception, JSON malformato, error inline).

Riferimento NLU + resilienza: stesso pattern di fallback chain gia' usato
per l'agente principale (`nexus_provider_default_model`).
"""
from __future__ import annotations

import asyncio
from dataclasses import dataclass
from typing import Any, List
from unittest.mock import AsyncMock, patch

import pytest

from brain.router.agentic_classifier import (
    AgenticIntentClassifier,
    AgenticIntent,
    ClassifierChainEntry,
    _load_classifier_chain,
)


# =========================================================================
# _load_classifier_chain: lettura DB-driven
# =========================================================================


def test_load_chain_senza_database_url_ritorna_vuoto() -> None:
    """DATABASE_URL non set → chain vuota (no panic, lascia caller fare
    fallback retrocompat su settings.routing.classifier_*)."""
    import os
    import brain.router.agentic_classifier as mod

    # Reset cache
    mod._CHAIN_CACHE = []
    mod._CHAIN_CACHE_EXPIRY = 0.0
    saved = os.environ.get("DATABASE_URL")
    os.environ["DATABASE_URL"] = ""
    try:
        chain = asyncio.run(_load_classifier_chain())
        assert chain == []
    finally:
        if saved is not None:
            os.environ["DATABASE_URL"] = saved
        else:
            os.environ.pop("DATABASE_URL", None)


def test_load_chain_db_irraggiungibile_ritorna_vuoto() -> None:
    """DB unreachable → lista vuota (caller usa singolo provider fallback)."""
    import os
    import brain.router.agentic_classifier as mod
    mod._CHAIN_CACHE = []
    mod._CHAIN_CACHE_EXPIRY = 0.0
    saved = os.environ.get("DATABASE_URL")
    os.environ["DATABASE_URL"] = "postgres://nope:nope@127.0.0.1:1/none"
    try:
        chain = asyncio.run(_load_classifier_chain())
        assert chain == []
    finally:
        if saved is not None:
            os.environ["DATABASE_URL"] = saved
        else:
            os.environ.pop("DATABASE_URL", None)


def test_classifier_chain_entry_dataclass() -> None:
    e = ClassifierChainEntry(provider="google", model="gemini-2.5-flash", priority=100)
    assert e.provider == "google"
    assert e.model == "gemini-2.5-flash"
    assert e.priority == 100


# =========================================================================
# classify() con chain: scenari di fallback
# =========================================================================


@dataclass
class FakeProviderResult:
    """Mock di un risultato provider per testare classify()."""
    content: str
    model: str = ""


class FakeProviderRegistry:
    """Registry di test che simula chiamate provider con scenari predefiniti.

    `script` e' una list di tuple (provider, model, action) dove action e':
    - ("ok", json_str): ritorna FakeProviderResult con content=json_str
    - ("error", msg): ritorna content='[Error: msg]' (inline error)
    - ("timeout",): solleva asyncio.TimeoutError
    - ("exception", exc_type_name): solleva exception del tipo dato
    - ("malformed", text): ritorna content malformato (no JSON valido)
    """

    def __init__(self, script: List[tuple]):
        self.script = list(script)
        self.calls: List[tuple[str, str]] = []

    async def generate_completion_async(
        self, provider: str, model: str, prompt: str
    ) -> FakeProviderResult:
        self.calls.append((provider, model))
        # Cerca il primo entry script che matcha (provider, model)
        for i, entry in enumerate(self.script):
            if entry[0] == provider and entry[1] == model:
                action = entry[2:]
                # consume
                self.script.pop(i)
                if action[0] == "ok":
                    return FakeProviderResult(content=action[1], model=model)
                if action[0] == "error":
                    return FakeProviderResult(content=f"[Error: {action[1]}]", model=model)
                if action[0] == "timeout":
                    raise asyncio.TimeoutError()
                if action[0] == "exception":
                    raise RuntimeError(action[1])
                if action[0] == "malformed":
                    return FakeProviderResult(content=action[1], model=model)
        raise RuntimeError(f"FakeProviderRegistry: no script for {provider}/{model}")


def _valid_json_for(intent: str = "debug") -> str:
    """JSON valido che _validate_parsed accetta."""
    return (
        '{"intent":"' + intent + '","agentic_score":0.9,"requires_tools":true,'
        '"complexity":"high","confidence":0.85,'
        '"candidates":[{"intent":"' + intent + '","confidence":0.85}],'
        '"slots":{"action_verb":"resolve","target_type":"tests",'
        '"framework":"playwright","scope":"multi_file","confidence":0.92}}'
    )


def _make_classifier_with_chain(
    chain: List[tuple[str, str]],
    script: List[tuple],
) -> tuple[AgenticIntentClassifier, FakeProviderRegistry]:
    """Costruisce classifier con chain custom e script di scenari."""
    registry = FakeProviderRegistry(script)
    classifier = AgenticIntentClassifier(
        provider_registry=registry,
        provider=chain[0][0],
        model=chain[0][1],
    )
    # Bypass _ensure_config: setto direttamente
    from brain.router.agentic_classifier import _TTLCache, DEFAULT_LLM_TIMEOUT_SECONDS
    classifier._cache = _TTLCache(max_entries=100, ttl_seconds=60)
    classifier._llm_timeout = 5.0
    classifier._ambiguity_min_confidence = 0.70
    classifier._ambiguity_min_margin = 0.15
    return classifier, registry


def _patch_chain(chain: List[tuple[str, str]]):
    """Patcha _load_classifier_chain per ritornare la chain di test."""
    entries = [
        ClassifierChainEntry(provider=p, model=m, priority=100 - i * 10)
        for i, (p, m) in enumerate(chain)
    ]

    async def fake_load():
        return entries

    return patch(
        "brain.router.agentic_classifier._load_classifier_chain",
        new=fake_load,
    )


# === Scenario A: primo provider OK → vince subito ===

def test_chain_primo_provider_ok_vince_subito() -> None:
    chain = [("google", "gemini-2.5-flash"), ("openai", "gpt-4.1-mini")]
    script = [("google", "gemini-2.5-flash", "ok", _valid_json_for("debug"))]
    classifier, registry = _make_classifier_with_chain(chain, script)
    with _patch_chain(chain):
        result = asyncio.run(classifier.classify("esegui i test playwright e correggi"))
    assert result.intent == "debug"
    assert result.fallback_used is False
    # Solo il primo provider chiamato
    assert registry.calls == [("google", "gemini-2.5-flash")]


# === Scenario B: primo provider fa inline error → fallback al secondo ===

def test_chain_primo_provider_inline_error_fallback_al_secondo() -> None:
    """Caso GEMINI THROTTLING: il provider ritorna '[Error: high demand]'
    invece di JSON. La chain deve cadere sul prossimo provider."""
    chain = [("google", "gemini-2.5-flash"), ("mistral", "mistral-small-latest")]
    script = [
        ("google", "gemini-2.5-flash", "error", "This model is currently experiencing high demand"),
        ("mistral", "mistral-small-latest", "ok", _valid_json_for("debug")),
    ]
    classifier, registry = _make_classifier_with_chain(chain, script)
    with _patch_chain(chain):
        result = asyncio.run(classifier.classify("esegui i test e risolvi"))
    assert result.intent == "debug"
    assert result.fallback_used is False  # un provider ha avuto successo
    # Entrambi i provider sono stati chiamati (primo fallito inline, secondo OK)
    assert registry.calls == [
        ("google", "gemini-2.5-flash"),
        ("mistral", "mistral-small-latest"),
    ]
    # model_used dell'AgenticIntent deve riflettere il provider VINCENTE
    assert "mistral-small-latest" in result.model_used


# === Scenario C: primo provider timeout → fallback al secondo ===

def test_chain_primo_provider_timeout_fallback_al_secondo() -> None:
    chain = [("google", "gemini-2.5-flash"), ("openai", "gpt-4.1-mini")]
    script = [
        ("google", "gemini-2.5-flash", "timeout"),
        ("openai", "gpt-4.1-mini", "ok", _valid_json_for("fix")),
    ]
    classifier, registry = _make_classifier_with_chain(chain, script)
    with _patch_chain(chain):
        result = asyncio.run(classifier.classify("test"))
    assert result.intent == "fix"
    assert len(registry.calls) == 2


# === Scenario D: primo JSON malformato → fallback al secondo ===

def test_chain_primo_json_malformato_fallback_al_secondo() -> None:
    chain = [("google", "gemini-2.5-flash"), ("openai", "gpt-4.1-mini")]
    script = [
        ("google", "gemini-2.5-flash", "malformed", "not valid json {{{"),
        ("openai", "gpt-4.1-mini", "ok", _valid_json_for("debug")),
    ]
    classifier, registry = _make_classifier_with_chain(chain, script)
    with _patch_chain(chain):
        result = asyncio.run(classifier.classify("test"))
    assert result.intent == "debug"
    assert len(registry.calls) == 2


# === Scenario E: 3 falliti su 4, ultimo OK ===

def test_chain_4_provider_solo_ultimo_ok() -> None:
    chain = [
        ("google", "gemini-2.5-flash"),
        ("mistral", "mistral-small-latest"),
        ("openai", "gpt-4.1-mini"),
        ("deepseek", "deepseek-chat"),
    ]
    script = [
        ("google", "gemini-2.5-flash", "error", "down"),
        ("mistral", "mistral-small-latest", "timeout"),
        ("openai", "gpt-4.1-mini", "malformed", "{nope"),
        ("deepseek", "deepseek-chat", "ok", _valid_json_for("test")),
    ]
    classifier, registry = _make_classifier_with_chain(chain, script)
    with _patch_chain(chain):
        result = asyncio.run(classifier.classify("test"))
    assert result.intent == "test"
    assert len(registry.calls) == 4
    assert "deepseek" in result.model_used


# === Scenario F: TUTTI falliscono → fallback keyword ===

def test_chain_tutti_provider_falliscono_fallback_keyword() -> None:
    """Se l'intera chain fallisce, classify ritorna risultato keyword
    fallback con fallback_used=True (mai eccezione)."""
    chain = [("google", "gemini-2.5-flash"), ("openai", "gpt-4.1-mini")]
    script = [
        ("google", "gemini-2.5-flash", "error", "down"),
        ("openai", "gpt-4.1-mini", "error", "down"),
    ]
    classifier, registry = _make_classifier_with_chain(chain, script)
    with _patch_chain(chain):
        result = asyncio.run(classifier.classify("imposta utente admin"))
    # Tutti provati
    assert len(registry.calls) == 2
    # Fallback path
    assert result.fallback_used is True
    # Reason contiene "chain_exhausted" prefisso
    assert "chain_exhausted" in result.model_used


# === Scenario G: chain vuota (DB down) → singolo fallback retrocompat ===

def test_chain_vuota_usa_singolo_provider_retrocompat() -> None:
    """Se _load_classifier_chain ritorna [] (DB down/tabella vuota),
    classify usa (self._provider, self._model) come chain a 1 elemento."""
    chain = [("anthropic", "claude-haiku-4-5-20251001")]  # solo per setup
    script = [("anthropic", "claude-haiku-4-5-20251001", "ok", _valid_json_for("docs"))]
    classifier, registry = _make_classifier_with_chain(chain, script)

    async def empty_chain():
        return []

    with patch(
        "brain.router.agentic_classifier._load_classifier_chain",
        new=empty_chain,
    ):
        result = asyncio.run(classifier.classify("scrivi docs"))
    assert result.intent == "docs"
    # Ha usato self._provider/_model dell'istanza (retrocompat)
    assert registry.calls == [("anthropic", "claude-haiku-4-5-20251001")]
