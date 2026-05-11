"""Test L4: slot filling + estrazione canonica + dataclass ActionSlots.

Verifica che il classifier Python:
- estrae correttamente i 4 slot dai JSON validi
- valida enumerati (action_verb, target_type, scope)
- gestisce framework come stringa libera + wildcard ""
- ritorna ActionSlots() vuoto su input malformato (no eccezioni)
- popola `slots` su AgenticIntent.to_dict() per il Rust consumer

Riferimento: docs/audit-token-reduction.md sezione "Livello 4 — Slot filling".
"""
from __future__ import annotations

import pytest

from brain.router.agentic_classifier import (
    AgenticIntentClassifier,
    AgenticIntent,
    ActionSlots,
    IntentCandidate,
    ALLOWED_ACTION_VERBS,
    ALLOWED_TARGET_TYPES,
    ALLOWED_SCOPES,
)


# =========================================================================
# Schema canonico (enumerativi)
# =========================================================================


def test_enumerativi_action_verb_completi() -> None:
    """Verifica lista canonica action_verbs (8 valori)."""
    expected = {
        "read", "write", "resolve", "analyze",
        "refactor", "configure", "deploy", "delete",
    }
    assert ALLOWED_ACTION_VERBS == expected


def test_enumerativi_target_type_completi() -> None:
    expected = {
        "code", "tests", "config", "service",
        "docs", "data", "infrastructure",
    }
    assert ALLOWED_TARGET_TYPES == expected


def test_enumerativi_scope_completi() -> None:
    expected = {"single", "multi_file", "cross_service", "system_wide"}
    assert ALLOWED_SCOPES == expected


# =========================================================================
# ActionSlots.is_complete() + meets_confidence()
# =========================================================================


def test_is_complete_richiede_3_campi_canonici() -> None:
    """framework e' opzionale; action_verb/target_type/scope obbligatori."""
    full = ActionSlots(
        action_verb="resolve", target_type="tests",
        framework="playwright", scope="multi_file", confidence=0.9,
    )
    assert full.is_complete() is True

    # framework vuoto e' OK (wildcard)
    no_fw = ActionSlots(
        action_verb="read", target_type="code",
        framework="", scope="single", confidence=0.9,
    )
    assert no_fw.is_complete() is True

    # action_verb mancante
    bad_av = ActionSlots(
        action_verb="", target_type="tests",
        framework="", scope="single", confidence=0.9,
    )
    assert bad_av.is_complete() is False

    # scope mancante
    bad_scope = ActionSlots(
        action_verb="resolve", target_type="tests",
        framework="", scope="", confidence=0.9,
    )
    assert bad_scope.is_complete() is False


def test_is_complete_rifiuta_action_verb_non_canonico() -> None:
    """Valori fuori da ALLOWED_ACTION_VERBS sono rifiutati."""
    invalid = ActionSlots(
        action_verb="hack_the_planet", target_type="code",
        framework="", scope="single", confidence=0.9,
    )
    assert invalid.is_complete() is False


def test_is_complete_rifiuta_scope_non_canonico() -> None:
    invalid = ActionSlots(
        action_verb="read", target_type="code",
        framework="", scope="entire_universe", confidence=0.9,
    )
    assert invalid.is_complete() is False


# =========================================================================
# _parse_slots: robustezza
# =========================================================================


def test_parse_slots_da_json_valido() -> None:
    parsed = {
        "slots": {
            "action_verb": "resolve",
            "target_type": "tests",
            "framework": "playwright",
            "scope": "multi_file",
            "confidence": 0.92,
        }
    }
    s = AgenticIntentClassifier._parse_slots(parsed)
    assert s.action_verb == "resolve"
    assert s.target_type == "tests"
    assert s.framework == "playwright"
    assert s.scope == "multi_file"
    assert s.confidence == 0.92
    assert s.is_complete()


def test_parse_slots_assente_ritorna_actionsslots_vuoto() -> None:
    """Se il JSON non ha `slots`, ritorna ActionSlots() default (no eccezioni).
    Il caller fa fallback al routing classico."""
    parsed = {"intent": "chat"}
    s = AgenticIntentClassifier._parse_slots(parsed)
    assert s.action_verb == ""
    assert not s.is_complete()


def test_parse_slots_action_verb_invalido_svuota_solo_il_campo() -> None:
    """Robustezza: se action_verb e' invalido, lo svuotiamo ma teniamo gli
    altri campi (potenzialmente usabili per debug)."""
    parsed = {
        "slots": {
            "action_verb": "MAGIC_HACK",  # invalido
            "target_type": "tests",
            "framework": "playwright",
            "scope": "multi_file",
            "confidence": 0.9,
        }
    }
    s = AgenticIntentClassifier._parse_slots(parsed)
    assert s.action_verb == ""  # svuotato
    assert s.target_type == "tests"  # preservato
    # NON e' complete perche' action_verb mancante
    assert not s.is_complete()


def test_parse_slots_normalizza_case_lowercase() -> None:
    """L'LLM puo' restituire valori in case misto; normalizziamo a lower."""
    parsed = {
        "slots": {
            "action_verb": "RESOLVE",
            "target_type": "Tests",
            "framework": "Playwright",
            "scope": "MULTI_FILE",
            "confidence": 0.9,
        }
    }
    s = AgenticIntentClassifier._parse_slots(parsed)
    assert s.action_verb == "resolve"
    assert s.target_type == "tests"
    assert s.framework == "playwright"
    assert s.scope == "multi_file"
    assert s.is_complete()


def test_parse_slots_clampa_confidence_in_range() -> None:
    """Valori fuori [0,1] vengono clampati."""
    parsed = {
        "slots": {
            "action_verb": "read", "target_type": "code",
            "framework": "", "scope": "single",
            "confidence": 1.5,  # > 1
        }
    }
    s = AgenticIntentClassifier._parse_slots(parsed)
    assert 0.0 <= s.confidence <= 1.0

    parsed_neg = {
        "slots": {
            "action_verb": "read", "target_type": "code",
            "framework": "", "scope": "single",
            "confidence": -0.5,
        }
    }
    s_neg = AgenticIntentClassifier._parse_slots(parsed_neg)
    assert 0.0 <= s_neg.confidence <= 1.0


def test_parse_slots_su_json_non_dict_ritorna_default() -> None:
    """Robustezza: slots come stringa/lista/null → default vuoto."""
    for bad in [None, "string", [], 42]:
        parsed = {"slots": bad}
        s = AgenticIntentClassifier._parse_slots(parsed)
        assert s.action_verb == ""
        assert s.confidence == 0.0


# =========================================================================
# _validate_parsed: integrazione slots
# =========================================================================


def test_validate_parsed_propaga_slots_a_agentic_intent() -> None:
    """End-to-end: JSON LLM completo → AgenticIntent con slots popolati."""
    parsed = {
        "intent": "debug",
        "agentic_score": 0.95,
        "requires_tools": True,
        "complexity": "high",
        "confidence": 0.85,
        "candidates": [{"intent": "debug", "confidence": 0.85}],
        "slots": {
            "action_verb": "resolve",
            "target_type": "tests",
            "framework": "playwright",
            "scope": "multi_file",
            "confidence": 0.92,
        },
    }
    result = AgenticIntentClassifier._validate_parsed(parsed)
    assert result is not None
    assert result.slots.action_verb == "resolve"
    assert result.slots.is_complete()


def test_validate_parsed_senza_slots_lascia_default_vuoto() -> None:
    """JSON LLM SENZA il campo slots → AgenticIntent ha slots vuoti
    (per backward compat con LLM che non hanno ancora il prompt esteso)."""
    parsed = {
        "intent": "chat",
        "agentic_score": 0.0,
        "requires_tools": False,
        "complexity": "low",
        "confidence": 0.99,
    }
    result = AgenticIntentClassifier._validate_parsed(parsed)
    assert result is not None
    assert result.slots.action_verb == ""
    assert not result.slots.is_complete()


# =========================================================================
# Serializzazione (per consumer Rust)
# =========================================================================


def test_to_dict_include_slots_serializzati() -> None:
    """AgenticIntent.to_dict() deve includere `slots` come dict per il
    deserialize del Rust AgenticIntentResponse."""
    intent = AgenticIntent(
        intent="debug", agentic_score=0.9, requires_tools=True,
        complexity="high", confidence=0.85, model_used="gemini-2.5-flash",
        slots=ActionSlots(
            action_verb="resolve", target_type="tests",
            framework="playwright", scope="multi_file", confidence=0.92,
        ),
    )
    d = intent.to_dict()
    assert "slots" in d
    assert isinstance(d["slots"], dict)
    assert d["slots"]["action_verb"] == "resolve"
    assert d["slots"]["framework"] == "playwright"
    assert d["slots"]["confidence"] == 0.92


# =========================================================================
# Casi golden: messaggi reali (parametrici)
# =========================================================================


@pytest.mark.parametrize(
    "msg,expected_action,expected_target,expected_scope,note",
    [
        # CASI PARADIGMATICI del bug Redemptor: test failure resolution
        ("esegui i test playwright e risolvi i fail",
         "resolve", "tests", "multi_file",
         "BUG ORIGINALE: prima andava su gpt-4.1-mini"),
        ("i test pytest non passano, correggi",
         "resolve", "tests", "multi_file",
         "verbo 'correggi' su test failure"),
        ("fai funzionare i test cargo",
         "resolve", "tests", "multi_file",
         "make work = resolve"),
        # CONTRO-CASI: scrittura nuovi test
        ("scrivi un test per la funzione foo",
         "write", "tests", "single",
         "scrittura singolo test"),
        ("aggiungi test pytest per il modulo auth",
         "write", "tests", "single",
         "aggiunta test"),
        # LETTURA file
        ("leggi src/main.py e dimmi cosa fa",
         "read", "code", "single",
         "lettura singola"),
        ("elenca i file nel progetto",
         "read", "code", "multi_file",
         "elenco file"),
        # DEBUG cross-service
        ("perche' il backend non risponde dal frontend?",
         "analyze", "service", "cross_service",
         "root cause cross-service"),
        # REFACTOR multi-file
        ("refactor del modulo auth in piu' file",
         "refactor", "code", "multi_file",
         "refactor multi-file"),
        # DEPLOY
        ("deploya il microservizio doc-service",
         "deploy", "service", "cross_service",
         "deploy servizio"),
        # DELETE (sicurezza)
        ("elimina i file dockerfile rimasti",
         "delete", "infrastructure", "multi_file",
         "eliminazione file"),
        # DOCS
        ("scrivi la documentazione per questa classe",
         "write", "docs", "single",
         "scrittura docs"),
        # CONFIGURE
        ("imposta un utente admin per l'applicazione",
         "configure", "service", "multi_file",
         "configurazione utente"),
    ],
)
def test_caso_golden_estrazione_slots_canonica(
    msg: str, expected_action: str, expected_target: str,
    expected_scope: str, note: str,
) -> None:
    """Test golden parametrico: simula la response LLM ideale per 13 casi
    reali e verifica che il parser estragga gli slot corretti.

    NOTA: questo NON e' un test end-to-end (non chiamiamo Gemini): simula
    quale slots l'LLM DOVREBBE ritornare per ogni input. L'effettivo
    comportamento LLM si valuta in produzione via `nexus_routing_decisions`
    e dashboard admin.
    """
    # Simula JSON che il classifier DOVREBBE ritornare
    parsed = {
        "intent": "debug",  # placeholder, non rilevante qui
        "agentic_score": 0.9,
        "requires_tools": True,
        "complexity": "medium",
        "confidence": 0.85,
        "slots": {
            "action_verb": expected_action,
            "target_type": expected_target,
            "framework": "",
            "scope": expected_scope,
            "confidence": 0.88,
        },
    }
    result = AgenticIntentClassifier._validate_parsed(parsed)
    assert result is not None, f"validate_parsed None per: {msg}"
    assert result.slots.is_complete(), f"slots incompleti per: {msg} ({note})"
    assert result.slots.action_verb == expected_action
    assert result.slots.target_type == expected_target
    assert result.slots.scope == expected_scope
