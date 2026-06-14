"""Test del trasporto unico (gateway) nel ProviderRegistry.

Dopo l'eliminazione della duplicazione adapter SDK (il flag
``brain.use_gateway_provider`` e' stato rimosso: il gateway e' l'unica via per le
chiamate LLM), il registry instrada SEMPRE generate / generate_agent_turn /
generate_completion al GatewayProvider, mantenendo qui la selezione
(provider, model) (routing/cooldown/cascade del brain).

Si verifica il contratto registry -> trasporto:
  - il GatewayProvider riceve il model nel formato ``provider/model``;
  - nessun adapter SDK viene usato per le chiamate;
  - ``_transport_model`` non raddoppia un prefisso gia' presente.

Lo split del prefisso in ``pin_provider``+``model`` concreto avviene DENTRO il
GatewayProvider reale (coperto da test_gateway_provider.py).

Idempotenti e auto-contenuti: i provider SDK reali e il GatewayProvider sono
sostituiti da doppi che catturano il model ricevuto; le funzioni che toccano il
DB (billing/cooldown/usage) sono neutralizzate via monkeypatch.
"""
from __future__ import annotations

from typing import Any

import pytest

from brain.providers import registry as reg
from brain.providers.base import BaseProvider, ProviderCatalogEntry, ProviderResult


# ──────────────────────────────────────────────────────────────────────────────
# Doppi di test (nessuna rete, nessun SDK reale)
# ──────────────────────────────────────────────────────────────────────────────


class _RecordingProvider(BaseProvider):
    """Provider fittizio che registra il ``model`` ricevuto dalle chiamate.

    Espone ``generate`` e ``generate_agent_turn`` con le stesse firme degli
    adapter reali (incluso ``**kwargs``), cosi' l'introspezione difensiva del
    registry e il trasporto si comportano come in produzione.
    """

    def __init__(self, name: str) -> None:
        self.name = name
        self.agent_models: list[str] = []
        self.generate_models: list[str] = []

    async def generate(self, model: str, prompt: str, **kwargs: Any) -> ProviderResult:
        self.generate_models.append(model)
        return ProviderResult(
            provider=self.name, model=model, content="ok-generate",
            metadata={"usage": {"prompt_tokens": 1, "completion_tokens": 1}},
        )

    async def generate_agent_turn(
        self,
        model: str,
        messages: list[dict],
        tools: list[dict],
        max_tokens: int = 4096,
        system_text: str = "",
        **kwargs: Any,
    ) -> ProviderResult:
        self.agent_models.append(model)
        # stop_reason end_turn con contenuto sopra soglia -> nessun fallback M4.
        return ProviderResult(
            provider=self.name, model=model,
            content="risposta completa del turno agentico ben oltre la soglia minima",
            metadata={"stop_reason": "end_turn", "usage": {"input_tokens": 1, "output_tokens": 1}},
        )

    async def test_connection(self) -> dict[str, Any]:  # pragma: no cover - non usato
        return {"provider": self.name, "status": "ready"}

    def list_models(self) -> list[ProviderCatalogEntry]:  # pragma: no cover - non usato
        return []


@pytest.fixture
def isolated_registry(monkeypatch: pytest.MonkeyPatch) -> reg.ProviderRegistry:
    """ProviderRegistry con i provider SDK sostituiti da doppi e i path DB
    (billing/cooldown/usage) neutralizzati. Nessuna rete, nessun DB."""
    registry = reg.ProviderRegistry()
    # Sostituisci gli adapter SDK reali con doppi che catturano il model.
    registry._providers = {name: _RecordingProvider(name) for name in registry._providers}

    # Neutralizza i path che toccherebbero il DB (best-effort gia', ma forziamo
    # l'isolamento totale dal DB e dalla rete in CI).
    monkeypatch.setattr(reg, "_record_usage", lambda *a, **k: None)
    monkeypatch.setattr(reg, "_record_intent_health", lambda *a, **k: None)
    monkeypatch.setattr(reg, "_is_in_billing_cooldown", lambda *a, **k: False)
    monkeypatch.setattr(reg, "_mark_billing_cooldown", lambda *a, **k: None)
    monkeypatch.setattr(reg, "_clear_billing_cooldown", lambda *a, **k: None)
    monkeypatch.setattr(reg, "_intent_in_cooldown", lambda *a, **k: False)
    monkeypatch.setattr(
        reg, "_enforce_quota_estimate", lambda *a, **k: (True, "")
    )
    return registry


# ──────────────────────────────────────────────────────────────────────────────
# Trasporto unico: tutte le chiamate LLM passano dal gateway col model prefissato
# ──────────────────────────────────────────────────────────────────────────────


def test_agent_turn_usa_gateway_con_model_prefissato(
    isolated_registry: reg.ProviderRegistry, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake_gateway = _RecordingProvider("gateway")
    monkeypatch.setattr(isolated_registry, "_gateway", lambda: fake_gateway)

    isolated_registry.generate_agent_turn_sync(
        provider="openai", model="gpt-4o-mini",
        messages=[{"role": "user", "content": "ciao"}], tools=[],
    )

    # Il GatewayProvider ha ricevuto il model prefissato "provider/model"...
    assert fake_gateway.agent_models == ["openai/gpt-4o-mini"]
    # ...e nessun adapter SDK e' stato chiamato.
    assert isolated_registry._providers["openai"].agent_models == []


def test_generate_completion_usa_gateway_con_model_prefissato(
    isolated_registry: reg.ProviderRegistry, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake_gateway = _RecordingProvider("gateway")
    monkeypatch.setattr(isolated_registry, "_gateway", lambda: fake_gateway)

    isolated_registry.generate_completion(
        provider="google", model="gemini-2.5-flash", prompt="ping",
    )

    assert fake_gateway.generate_models == ["google/gemini-2.5-flash"]
    assert isolated_registry._providers["google"].generate_models == []


@pytest.mark.asyncio
async def test_generate_completion_async_usa_gateway_con_model_prefissato(
    isolated_registry: reg.ProviderRegistry, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Anche il path async (classifier/summarizer/next_actions) instrada al
    gateway: prima dell'eliminazione del flag bypassava il bivio trasporto."""
    fake_gateway = _RecordingProvider("gateway")
    monkeypatch.setattr(isolated_registry, "_gateway", lambda: fake_gateway)

    await isolated_registry.generate_completion_async(
        provider="deepseek", model="deepseek-chat", prompt="ping",
    )

    assert fake_gateway.generate_models == ["deepseek/deepseek-chat"]
    assert isolated_registry._providers["deepseek"].generate_models == []


def test_transport_model_non_raddoppia_il_prefisso() -> None:
    """Se il routing passa gia' un model prefissato, il gateway non lo raddoppia."""
    assert (
        reg.ProviderRegistry._transport_model("openai", "openai/gpt-4o-mini")
        == "openai/gpt-4o-mini"
    )


def test_transport_for_e_sempre_il_gateway(
    isolated_registry: reg.ProviderRegistry, monkeypatch: pytest.MonkeyPatch
) -> None:
    """``_transport_for`` ritorna sempre il GatewayProvider, qualunque sia il
    provider richiesto: il trasporto e' unico (regola L)."""
    fake_gateway = _RecordingProvider("gateway")
    monkeypatch.setattr(isolated_registry, "_gateway", lambda: fake_gateway)
    assert isolated_registry._transport_for("openai") is fake_gateway
    assert isolated_registry._transport_for("anthropic") is fake_gateway
    assert isolated_registry._transport_for("provider-inesistente") is fake_gateway
