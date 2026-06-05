"""Coerenza tra il punto unico degli intent e le altre liste correlate.

Cattura il drift descritto da regola L / ADR 0026: se un nuovo intent entra in
``ALLOWED_INTENTS`` ma nessuno aggiorna gli exemplars, o viceversa, questo test
si rompe e chi modifica viene forzato a guardare entrambi i posti.
"""
from brain.router.intents import ALLOWED_COMPLEXITY, ALLOWED_INTENTS


def test_allowed_intents_non_vuoto():
    assert isinstance(ALLOWED_INTENTS, frozenset)
    assert len(ALLOWED_INTENTS) >= 8


def test_complessita_canoniche():
    assert ALLOWED_COMPLEXITY == frozenset({"low", "medium", "high"})


def test_agentic_classifier_riusa_il_punto_unico():
    from brain.router import agentic_classifier as ac
    assert ac.ALLOWED_INTENTS is ALLOWED_INTENTS
    assert ac.ALLOWED_COMPLEXITY is ALLOWED_COMPLEXITY


def test_intent_exemplars_chiavi_documentate():
    """Le chiavi degli exemplars hanno scopo diverso (training set degli
    embedding), ma quando entrambe le liste cambiano va aggiornata anche questa
    documentazione: e' il modo in cui la regola L si autodifende dal drift.

    Drift noto e accettato al momento del consolidamento:
      - 'database_schema_change' compare negli exemplars ma non in
        ALLOWED_INTENTS (intent non ancora promosso al classifier agentico);
      - 'debug' compare in ALLOWED_INTENTS ma non negli exemplars (cade su
        'fix' nel router semantico, intenzionale).
    """
    from brain.router.service import _INTENT_EXEMPLARS

    diff_solo_exemplars = set(_INTENT_EXEMPLARS) - set(ALLOWED_INTENTS)
    diff_solo_allowed = set(ALLOWED_INTENTS) - set(_INTENT_EXEMPLARS)
    assert diff_solo_exemplars == {"database_schema_change"}, (
        f"Drift exemplars vs allowed cambiato: {diff_solo_exemplars}. "
        "Aggiornare ALLOWED_INTENTS in intents.py o questo test."
    )
    assert diff_solo_allowed == {"debug"}, (
        f"Drift allowed vs exemplars cambiato: {diff_solo_allowed}. "
        "Aggiornare _INTENT_EXEMPLARS in service.py o questo test."
    )
