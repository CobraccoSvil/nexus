"""Test L1+L2: ambiguity detection + candidates nel classifier agentico.

Verifica che `AgenticIntentClassifier._validate_parsed()` rilevi correttamente
i casi ambigui (confidence bassa, margine stretto) e popoli la lista di
candidati per consentire la disambiguazione lato Rust.

Riferimento NLU: Rasa/Dialogflow/LUIS best practice — quando il classifier
non e' sicuro, NON indovinare, chiedi chiarimenti.
"""
from __future__ import annotations

import pytest

from brain.router.agentic_classifier import (
    AgenticIntentClassifier,
    AgenticIntent,
    IntentCandidate,
    # Default tecnici (le soglie autoritative stanno in DB: mig 0132).
    DEFAULT_AMBIGUITY_MIN_CONFIDENCE,
    DEFAULT_AMBIGUITY_MIN_MARGIN,
)

# Alias retro-compatibili per i test (le soglie default coincidono col DB
# popolato in mig 0132 a 0.70 / 0.15).
AMBIGUITY_MIN_CONFIDENCE = DEFAULT_AMBIGUITY_MIN_CONFIDENCE
AMBIGUITY_MIN_MARGIN = DEFAULT_AMBIGUITY_MIN_MARGIN


# =========================================================================
# _is_ambiguous: 3 path (confidence bassa, margine stretto, normale)
# =========================================================================


def test_ambiguous_se_top_confidence_sotto_soglia() -> None:
    """Top confidence < 0.70 → ambiguo, anche se margine ampio."""
    candidates = [
        IntentCandidate(intent="debug", confidence=0.55),
        IntentCandidate(intent="chat", confidence=0.20),
    ]
    assert AgenticIntentClassifier._is_ambiguous(candidates) is True


def test_ambiguous_se_margine_sul_secondo_troppo_stretto() -> None:
    """Top alto ma secondo candidato vicinissimo → ambiguo.
    Caso paradigmatico: "i playwright sono rotti" → debug(0.55) vs fix(0.50)."""
    candidates = [
        IntentCandidate(intent="debug", confidence=0.85),
        IntentCandidate(intent="fix", confidence=0.80),
    ]
    # margin = 0.05 < AMBIGUITY_MIN_MARGIN (0.15)
    assert AgenticIntentClassifier._is_ambiguous(candidates) is True


def test_non_ambiguous_quando_confidence_alta_e_margine_ampio() -> None:
    """Decisione netta: top >> second."""
    candidates = [
        IntentCandidate(intent="code_read", confidence=0.95),
        IntentCandidate(intent="docs", confidence=0.30),
    ]
    assert AgenticIntentClassifier._is_ambiguous(candidates) is False


def test_non_ambiguous_con_solo_un_candidato_alto() -> None:
    """Top alto e nessun altro candidato → decisione netta."""
    candidates = [IntentCandidate(intent="chat", confidence=0.99)]
    assert AgenticIntentClassifier._is_ambiguous(candidates) is False


def test_ambiguous_se_lista_candidati_vuota() -> None:
    """Edge case: lista vuota → ambiguo per default (safety)."""
    assert AgenticIntentClassifier._is_ambiguous([]) is True


# =========================================================================
# _parse_candidates: robustezza al JSON malformato
# =========================================================================


def test_parse_candidates_da_json_valido() -> None:
    """Lista valida → tutti i candidati estratti, ordinati per confidence DESC."""
    parsed = {
        "candidates": [
            {"intent": "fix", "confidence": 0.70},
            {"intent": "debug", "confidence": 0.85},  # piu' alto, deve diventare primo
            {"intent": "test", "confidence": 0.30},
        ]
    }
    out = AgenticIntentClassifier._parse_candidates(parsed, "debug", 0.85)
    assert len(out) == 3
    assert out[0].intent == "debug"
    assert out[1].intent == "fix"
    assert out[2].intent == "test"
    # Sortati DESC
    assert out[0].confidence >= out[1].confidence >= out[2].confidence


def test_parse_candidates_assente_ritorna_solo_top() -> None:
    """Se il JSON non ha `candidates`, costruiamo una lista mono-elemento."""
    parsed = {"intent": "debug"}  # solo intent, niente candidates
    out = AgenticIntentClassifier._parse_candidates(parsed, "debug", 0.85)
    assert len(out) == 1
    assert out[0].intent == "debug"
    assert out[0].confidence == 0.85


def test_parse_candidates_scarta_intent_invalidi() -> None:
    """Se candidates contiene un intent non in ALLOWED_INTENTS, lo scarta."""
    parsed = {
        "candidates": [
            {"intent": "debug", "confidence": 0.80},
            {"intent": "magic_intent_unknown", "confidence": 0.70},
            {"intent": "fix", "confidence": 0.60},
        ]
    }
    out = AgenticIntentClassifier._parse_candidates(parsed, "debug", 0.80)
    intents = [c.intent for c in out]
    assert "magic_intent_unknown" not in intents
    assert intents == ["debug", "fix"]


def test_parse_candidates_clampa_confidence_in_range() -> None:
    """Valori fuori [0,1] vengono clampati."""
    parsed = {
        "candidates": [
            {"intent": "fix", "confidence": 1.5},  # > 1 → clamp a 1.0
            {"intent": "debug", "confidence": -0.3},  # < 0 → clamp a 0.0
        ]
    }
    out = AgenticIntentClassifier._parse_candidates(parsed, "fix", 0.99)
    assert all(0.0 <= c.confidence <= 1.0 for c in out)


def test_parse_candidates_max_3() -> None:
    """Anche se LLM ritorna piu' di 3, ne prendiamo max 3 (top)."""
    parsed = {
        "candidates": [
            {"intent": "debug", "confidence": 0.90},
            {"intent": "fix", "confidence": 0.70},
            {"intent": "test", "confidence": 0.50},
            {"intent": "chat", "confidence": 0.30},
            {"intent": "docs", "confidence": 0.10},
        ]
    }
    out = AgenticIntentClassifier._parse_candidates(parsed, "debug", 0.90)
    assert len(out) == 3


# =========================================================================
# _validate_parsed: integration test
# =========================================================================


def test_validate_parsed_popola_candidates_e_ambiguity() -> None:
    """End-to-end: LLM ritorna JSON completo → AgenticIntent con campi nuovi."""
    parsed = {
        "intent": "debug",
        "agentic_score": 0.85,
        "requires_tools": True,
        "complexity": "high",
        "confidence": 0.55,  # < AMBIGUITY_MIN_CONFIDENCE → ambiguo
        "candidates": [
            {"intent": "debug", "confidence": 0.55},
            {"intent": "fix", "confidence": 0.45},
        ],
    }
    result = AgenticIntentClassifier._validate_parsed(parsed)
    assert result is not None
    assert result.intent == "debug"
    assert result.is_ambiguous is True  # confidence bassa
    assert len(result.candidates) == 2


def test_validate_parsed_caso_non_ambiguo() -> None:
    """LLM molto sicuro → is_ambiguous=False."""
    parsed = {
        "intent": "chat",
        "agentic_score": 0.0,
        "requires_tools": False,
        "complexity": "low",
        "confidence": 0.99,
        "candidates": [{"intent": "chat", "confidence": 0.99}],
    }
    result = AgenticIntentClassifier._validate_parsed(parsed)
    assert result is not None
    assert result.is_ambiguous is False


def test_validate_parsed_caso_margine_stretto() -> None:
    """Top high confidence ma secondo candidato vicino → ambiguo."""
    parsed = {
        "intent": "debug",
        "agentic_score": 0.9,
        "requires_tools": True,
        "complexity": "high",
        "confidence": 0.80,
        "candidates": [
            {"intent": "debug", "confidence": 0.80},
            {"intent": "fix", "confidence": 0.75},  # margine 0.05 < 0.15
        ],
    }
    result = AgenticIntentClassifier._validate_parsed(parsed)
    assert result is not None
    assert result.is_ambiguous is True, "margine 0.05 sotto AMBIGUITY_MIN_MARGIN"


def test_validate_parsed_ritorna_none_su_intent_invalido() -> None:
    parsed = {
        "intent": "magic_unknown_intent",
        "agentic_score": 0.5,
        "requires_tools": True,
        "complexity": "low",
        "confidence": 0.9,
    }
    assert AgenticIntentClassifier._validate_parsed(parsed) is None


# =========================================================================
# Constants check
# =========================================================================


def test_soglie_disambiguation_in_range_ragionevole() -> None:
    """Le soglie devono essere sensate (non 0, non 1).
    Valori troppo bassi rendono il sistema sempre ambiguo;
    troppo alti rendono la disambiguazione inutile."""
    assert 0.5 < AMBIGUITY_MIN_CONFIDENCE < 0.95
    assert 0.05 < AMBIGUITY_MIN_MARGIN < 0.5


# =========================================================================
# AgenticIntent.to_dict serialization (per Rust deserialize)
# =========================================================================


def test_is_ambiguous_accetta_soglie_db_driven_via_parametri() -> None:
    """Le soglie sono DB-driven (mig 0132). _is_ambiguous deve accettarle
    via parametri invece di leggere costanti module-level."""
    # Stesso input, soglie diverse → risultato diverso
    candidates = [
        IntentCandidate(intent="debug", confidence=0.75),
        IntentCandidate(intent="fix", confidence=0.60),
    ]
    # Con soglia confidence 0.70 (default): non ambiguo (0.75 > 0.70, margine 0.15)
    assert (
        AgenticIntentClassifier._is_ambiguous(
            candidates, min_confidence=0.70, min_margin=0.15
        )
        is False
    )
    # Con soglia confidence alzata a 0.80: ambiguo (0.75 < 0.80)
    assert (
        AgenticIntentClassifier._is_ambiguous(
            candidates, min_confidence=0.80, min_margin=0.15
        )
        is True
    )
    # Con margine alzato a 0.20: ambiguo (margine 0.15 < 0.20)
    assert (
        AgenticIntentClassifier._is_ambiguous(
            candidates, min_confidence=0.70, min_margin=0.20
        )
        is True
    )


def test_validate_parsed_propaga_soglie_db_driven() -> None:
    """_validate_parsed deve accettare le soglie come parametri e
    propagarle a _is_ambiguous. Coerenza con DB-driven pattern."""
    parsed = {
        "intent": "fix",
        "agentic_score": 0.8,
        "requires_tools": True,
        "complexity": "medium",
        "confidence": 0.72,  # > default 0.70 → non ambiguo con default
        "candidates": [{"intent": "fix", "confidence": 0.72}],
    }
    # Con default: non ambiguo
    result_default = AgenticIntentClassifier._validate_parsed(parsed)
    assert result_default is not None
    assert result_default.is_ambiguous is False
    # Con soglia confidence alzata a 0.80: ambiguo
    result_strict = AgenticIntentClassifier._validate_parsed(
        parsed, ambiguity_min_confidence=0.80
    )
    assert result_strict is not None
    assert result_strict.is_ambiguous is True


def test_operational_defaults_includono_soglie_ambiguity() -> None:
    """_classifier_operational_defaults deve includere le 2 chiavi mig 0132
    per il ricovero parziale (se DB ha solo provider/model)."""
    from brain.router.agentic_classifier import _classifier_operational_defaults
    defs = _classifier_operational_defaults()
    assert "routing.ambiguity_min_confidence" in defs
    assert "routing.ambiguity_min_margin" in defs
    # Provider/model NON devono essere qui (sono autoritativamente in DB)
    assert "routing.classifier_provider" not in defs
    assert "routing.classifier_model" not in defs


def test_costanti_modulo_rimosse_solo_defaults() -> None:
    """Le vecchie costanti AMBIGUITY_MIN_* non devono piu' esistere come
    module-level. Solo i DEFAULT_AMBIGUITY_MIN_* sono validi (mig 0132)."""
    import brain.router.agentic_classifier as mod
    # Sono rimosse
    assert not hasattr(mod, "AMBIGUITY_MIN_CONFIDENCE"), (
        "AMBIGUITY_MIN_CONFIDENCE deve essere rimossa (DB-driven, mig 0132)"
    )
    assert not hasattr(mod, "AMBIGUITY_MIN_MARGIN"), (
        "AMBIGUITY_MIN_MARGIN deve essere rimossa (DB-driven, mig 0132)"
    )
    # Default tecnici esistono (per fallback DB down)
    assert hasattr(mod, "DEFAULT_AMBIGUITY_MIN_CONFIDENCE")
    assert hasattr(mod, "DEFAULT_AMBIGUITY_MIN_MARGIN")


def test_agentic_intent_to_dict_include_candidates_e_is_ambiguous() -> None:
    """Il dict serializzato deve includere i nuovi campi: il response
    `/classify-intent-agentic` viene letto da Rust che si aspetta queste chiavi."""
    intent = AgenticIntent(
        intent="debug",
        agentic_score=0.9,
        requires_tools=True,
        complexity="high",
        confidence=0.85,
        model_used="gemini-2.5-flash",
        candidates=[
            IntentCandidate(intent="debug", confidence=0.85),
            IntentCandidate(intent="fix", confidence=0.40),
        ],
        is_ambiguous=False,
    )
    d = intent.to_dict()
    assert "candidates" in d
    assert "is_ambiguous" in d
    assert d["is_ambiguous"] is False
    # Candidates devono essere serializzati come list di dict (non oggetti)
    assert isinstance(d["candidates"], list)
    assert d["candidates"][0]["intent"] == "debug"
    assert d["candidates"][0]["confidence"] == 0.85
