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


def test_agentic_default_e_intent_di_sistema():
    # L'intent neutro di fallback (LLM down) deve essere ammesso, cosi' il
    # classifier/routing non lo scarta come sconosciuto.
    assert "agentic_default" in ALLOWED_INTENTS


# NOTA: il vecchio test su _INTENT_EXEMPLARS e' stato RIMOSSO con il classifier
# keyword/embedding. La fonte di verita' degli intent e' ora ALLOWED_INTENTS +
# la routing matrix DB (nexus_routing_matrix); l'interpretazione e' solo LLM.
