"""Test sulla rimozione dei magic fallback (regola G di CLAUDE.md).

Verifica che quando il DB non e' disponibile o `nexus_purpose_model` /
`settings.routing.*` mancano, il codice sollevi un'eccezione esplicita
invece di silently-fallback a un modello hardcoded.

Pre-modifica:
  - agentic_classifier.py:44-45  → DEFAULT_CLASSIFIER_MODEL="gemini-2.5-flash"
  - summarizer.py:251-257       → mappa hardcoded {anthropic: claude-haiku, ...}

Post-modifica:
  - agentic_classifier._load_classifier_config() → raises ClassifierConfigUnavailable
  - summarizer._resolve_summary_model()           → raises SummaryModelUnavailable

Cosi' un cambio silenzioso di provider/modello e' impossibile per design.
"""
from __future__ import annotations

import os
from unittest.mock import patch

import pytest


# =========================================================================
# agentic_classifier: ClassifierConfigUnavailable
# =========================================================================


def test_classifier_config_unavailable_e_eccezione_dedicata() -> None:
    """L'eccezione e' una classe pubblica nel modulo, importabile dai caller
    che vogliono distinguerla da altre exception."""
    from brain.router.agentic_classifier import ClassifierConfigUnavailable
    assert issubclass(ClassifierConfigUnavailable, Exception)


def test_load_classifier_config_solleva_se_database_url_assente() -> None:
    """Se DATABASE_URL non e' impostata, _load_classifier_config solleva
    ClassifierConfigUnavailable invece di ritornare il vecchio default
    'google/gemini-2.5-flash'."""
    import asyncio
    from brain.router.agentic_classifier import (
        _load_classifier_config,
        ClassifierConfigUnavailable,
        _CONFIG_CACHE,
    )
    # Pulisci cache per forzare reload
    _CONFIG_CACHE.clear()
    with patch.dict(os.environ, {"DATABASE_URL": ""}, clear=False):
        with pytest.raises(ClassifierConfigUnavailable) as exc_info:
            asyncio.run(_load_classifier_config())
        assert "DATABASE_URL" in str(exc_info.value)


def test_load_classifier_config_solleva_se_db_irraggiungibile() -> None:
    """DB unreachable (porta chiusa) → ClassifierConfigUnavailable.
    Niente silenzioso fallback a gemini-2.5-flash."""
    import asyncio
    from brain.router.agentic_classifier import (
        _load_classifier_config,
        ClassifierConfigUnavailable,
        _CONFIG_CACHE,
    )
    _CONFIG_CACHE.clear()
    bogus_url = "postgres://nexus:nexus@127.0.0.1:1/nexus?sslmode=disable"
    with patch.dict(os.environ, {"DATABASE_URL": bogus_url}, clear=False):
        with pytest.raises(ClassifierConfigUnavailable) as exc_info:
            asyncio.run(_load_classifier_config())
        # Messaggio deve riferirsi al problema DB, non al fallback
        msg = str(exc_info.value).lower()
        assert "db" in msg or "irraggiungibile" in msg


def test_classifier_operational_defaults_non_contiene_modelli() -> None:
    """Il dict di default operativi NON deve contenere chiavi provider/model.
    Solo TTL, max_entries, timeout — parametri tecnici, non scelte di modello.

    Questa e' la garanzia strutturale: anche se qualcuno reintroduce un
    fallback, non puo' farlo passando da `_classifier_operational_defaults()`.
    """
    from brain.router.agentic_classifier import _classifier_operational_defaults
    defaults = _classifier_operational_defaults()
    # Chiavi forbidden
    assert "routing.classifier_provider" not in defaults
    assert "routing.classifier_model" not in defaults
    # Chiavi attese
    assert "routing.classifier_cache_ttl_seconds" in defaults
    assert "routing.classifier_cache_max_entries" in defaults
    assert "routing.llm_classifier_timeout_seconds" in defaults
    # Verifica che i valori siano stringhe parsabili a numero
    for k, v in defaults.items():
        assert isinstance(v, str)
        # I valori operativi devono essere numerici
        float(v)  # solleverebbe se non lo fosse


def test_modulo_non_esporta_piu_default_classifier_provider() -> None:
    """Verifica strutturale: le costanti DEFAULT_CLASSIFIER_PROVIDER e
    DEFAULT_CLASSIFIER_MODEL sono state rimosse dal modulo."""
    import brain.router.agentic_classifier as mod
    assert not hasattr(mod, "DEFAULT_CLASSIFIER_PROVIDER"), (
        "DEFAULT_CLASSIFIER_PROVIDER deve essere rimosso (regola G)"
    )
    assert not hasattr(mod, "DEFAULT_CLASSIFIER_MODEL"), (
        "DEFAULT_CLASSIFIER_MODEL deve essere rimosso (regola G)"
    )


# =========================================================================
# summarizer: SummaryModelUnavailable
# =========================================================================


def test_summary_model_unavailable_e_eccezione_dedicata() -> None:
    from brain.agents.summarizer import SummaryModelUnavailable
    assert issubclass(SummaryModelUnavailable, Exception)


def test_resolve_summary_model_solleva_se_database_url_assente() -> None:
    """Senza DATABASE_URL → eccezione, non piu' mappa hardcoded."""
    from brain.agents.summarizer import (
        _resolve_summary_model,
        SummaryModelUnavailable,
    )
    with patch.dict(os.environ, {"DATABASE_URL": ""}, clear=False):
        with pytest.raises(SummaryModelUnavailable) as exc_info:
            _resolve_summary_model("anthropic")
        assert "DATABASE_URL" in str(exc_info.value)


def test_resolve_summary_model_solleva_se_db_irraggiungibile() -> None:
    """DB unreachable → SummaryModelUnavailable.
    Niente silenzioso fallback a claude-haiku-4-5-20251001."""
    from brain.agents.summarizer import (
        _resolve_summary_model,
        SummaryModelUnavailable,
    )
    bogus_url = "postgres://nexus:nexus@127.0.0.1:1/nexus?sslmode=disable"
    with patch.dict(os.environ, {"DATABASE_URL": bogus_url}, clear=False):
        with pytest.raises(SummaryModelUnavailable) as exc_info:
            _resolve_summary_model("anthropic")
        msg = str(exc_info.value).lower()
        assert "db" in msg or "irraggiungibile" in msg


def test_resolve_summary_model_non_ritorna_mai_stringhe_hardcoded() -> None:
    """Smoke test: per ogni provider, la funzione o ritorna il modello dal DB
    o solleva eccezione. NON deve mai ritornare le vecchie stringhe hardcoded
    senza passare dal DB."""
    from brain.agents.summarizer import (
        _resolve_summary_model,
        SummaryModelUnavailable,
    )
    forbidden_hardcoded_values = {
        "claude-haiku-4-5-20251001",
        "gpt-4o-mini",
        "gemini-2.5-flash",
        "deepseek-chat",
        "mistral-small-latest",
    }
    # Forziamo DB irraggiungibile per garantire che il flusso DB venga
    # eseguito (no cache, no path veloce)
    bogus_url = "postgres://nexus:nexus@127.0.0.1:1/nexus?sslmode=disable"
    with patch.dict(os.environ, {"DATABASE_URL": bogus_url}, clear=False):
        for provider in ["anthropic", "openai", "google", "deepseek", "mistral", "ignoto"]:
            # Deve sollevare — NON ritornare un default magico
            with pytest.raises(SummaryModelUnavailable):
                result = _resolve_summary_model(provider)
                # Se non solleva (regressione), verifichiamo almeno che il
                # valore NON sia uno dei forbidden hardcoded
                assert result not in forbidden_hardcoded_values, (
                    f"regressione: _resolve_summary_model('{provider}') "
                    f"ha ritornato il valore hardcoded '{result}'"
                )


def test_modulo_summarizer_non_contiene_piu_mappa_fallback_hardcoded() -> None:
    """Verifica strutturale: leggendo il sorgente del modulo, non deve
    contenere piu' il dizionario di fallback `{anthropic: claude-haiku, ...}`."""
    import inspect
    import brain.agents.summarizer as mod
    src = inspect.getsource(mod._resolve_summary_model)
    # Pattern caratteristico della vecchia mappa hardcoded
    assert "claude-haiku-4-5-20251001" not in src or "fallback.get" not in src, (
        "regressione: _resolve_summary_model contiene ancora la mappa hardcoded"
    )
    # Pattern del nuovo comportamento: deve sollevare SummaryModelUnavailable
    assert "SummaryModelUnavailable" in src, (
        "regressione: _resolve_summary_model non solleva SummaryModelUnavailable"
    )
