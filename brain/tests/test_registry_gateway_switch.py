"""Test del bivio trasporto SDK vs gateway nel ProviderRegistry (passo 4b).

Verifica, SENZA rete e SENZA DB, che il feature flag ``brain.use_gateway_provider``
governi quale TRASPORTO esegue la chiamata, mantenendo invariati il routing e la
selezione (provider, model) del registry:

  - flag OFF -> usa l'adapter SDK registrato col model ORIGINALE (comportamento
    attuale, non rotto);
  - flag ON  -> usa il GatewayProvider (trasporto) col model nel formato
    ``provider/model``. Lo split di quel prefisso in ``pin_provider``+``model``
    concreto avviene DENTRO il GatewayProvider reale (coperto da
    test_gateway_provider.py): qui si verifica solo il contratto registry ->
    trasporto, cioe' che il registry passi ``provider/model`` al trasporto.

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
    registry e il bivio trasporto si comportano come in produzione.
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
# Flag OFF: comportamento attuale (SDK), model invariato
# ──────────────────────────────────────────────────────────────────────────────


def test_agent_turn_flag_off_usa_sdk_con_model_originale(
    isolated_registry: reg.ProviderRegistry, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(reg, "_use_gateway_provider", lambda: False)
    # Sentinella: se il registry costruisse il gateway, fallirebbe il test.
    monkeypatch.setattr(
        isolated_registry, "_gateway",
        lambda: pytest.fail("il gateway non deve essere usato con flag OFF"),
    )

    res = isolated_registry.generate_agent_turn_sync(
        provider="openai", model="gpt-4o-mini",
        messages=[{"role": "user", "content": "ciao"}], tools=[],
    )

    sdk = isolated_registry._providers["openai"]
    assert isinstance(sdk, _RecordingProvider)
    # L'adapter SDK ha ricevuto il model ORIGINALE, senza prefisso provider.
    assert sdk.agent_models == ["gpt-4o-mini"]
    assert res.content.startswith("risposta completa")


def test_generate_completion_flag_off_usa_sdk_con_model_originale(
    isolated_registry: reg.ProviderRegistry, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(reg, "_use_gateway_provider", lambda: False)

    isolated_registry.generate_completion(
        provider="deepseek", model="deepseek-chat", prompt="ping",
    )

    sdk = isolated_registry._providers["deepseek"]
    assert isinstance(sdk, _RecordingProvider)
    assert sdk.generate_models == ["deepseek-chat"]


# ──────────────────────────────────────────────────────────────────────────────
# Flag ON: trasporto gateway, model nel formato "provider/model"
# ──────────────────────────────────────────────────────────────────────────────


def test_agent_turn_flag_on_usa_gateway_con_model_prefissato(
    isolated_registry: reg.ProviderRegistry, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(reg, "_use_gateway_provider", lambda: True)
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


def test_generate_completion_flag_on_usa_gateway_con_model_prefissato(
    isolated_registry: reg.ProviderRegistry, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(reg, "_use_gateway_provider", lambda: True)
    fake_gateway = _RecordingProvider("gateway")
    monkeypatch.setattr(isolated_registry, "_gateway", lambda: fake_gateway)

    isolated_registry.generate_completion(
        provider="google", model="gemini-2.5-flash", prompt="ping",
    )

    assert fake_gateway.generate_models == ["google/gemini-2.5-flash"]
    assert isolated_registry._providers["google"].generate_models == []


def test_transport_model_non_raddoppia_il_prefisso(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Se il routing passa gia' un model prefissato, il gateway non lo raddoppia."""
    monkeypatch.setattr(reg, "_use_gateway_provider", lambda: True)
    assert (
        reg.ProviderRegistry._transport_model("openai", "openai/gpt-4o-mini")
        == "openai/gpt-4o-mini"
    )


def test_transport_model_flag_off_lascia_model_invariato(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(reg, "_use_gateway_provider", lambda: False)
    assert (
        reg.ProviderRegistry._transport_model("openai", "gpt-4o-mini") == "gpt-4o-mini"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Default del flag: assente dal DB -> False (comportamento attuale)
# ──────────────────────────────────────────────────────────────────────────────


def test_flag_default_false_se_db_non_disponibile(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Senza il flag in DB (o DB down) il trasporto resta SDK (default sicuro)."""
    import brain.utils.settings_db as settings_db

    def _boom(*a: Any, **k: Any) -> bool:
        raise RuntimeError("DB non disponibile")

    monkeypatch.setattr(settings_db, "get_bool_setting_cached", _boom)
    assert reg._use_gateway_provider() is False
