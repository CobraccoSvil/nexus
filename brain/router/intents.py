"""Intent canonici di routing: punto unico (regola L / ADR 0026).

Prima ``ALLOWED_INTENTS`` viveva hardcoded in ``router/agentic_classifier.py``;
``_INTENT_EXEMPLARS`` (set di frasi di esempio per il classifier embedding-based)
vive ancora in ``router/service.py`` perche' ha scopo diverso (training di
similarity), ma le sue chiavi devono restare coerenti con questa lista. Il drift
fra le due e' coperto dal test ``brain/tests/test_intents_consistency.py``.

Regola G (TODO): a regime la fonte autoritativa degli intent dovrebbe essere la
tabella DB ``nexus_routing_matrix`` (gia' query unica in Rust); finche' i call
site non sono pronti a leggere dal DB con cache, la lista vive qui come
single-source per i moduli Python.
"""
from __future__ import annotations

from typing import FrozenSet

# Intent ammessi dal classifier agentico. Devono coincidere con la colonna
# `intent_key` di ``nexus_routing_matrix``: un intent qui che non esiste in DB
# verrebbe accettato dal classifier ma non avrebbe un routing model -> errore
# runtime. Mantieni allineato.
ALLOWED_INTENTS: FrozenSet[str] = frozenset({
    "chat",
    "debug",
    "fix",
    "refactor",
    "test",
    "docs",
    "architecture",
    "file_ops",
    "system_admin",
    "code_read",
    # Intent di SISTEMA, non emesso dal classifier LLM: usato come fallback
    # neutro quando l'interpretazione semantica non e' disponibile (LLM down).
    # Attiva il _LAZY_MINIMAL_TOOLKIT (discovery + lettura) e modelli tool-robust
    # cosi' e' l'agente stesso a interpretare e agire. Vedi mig 0337.
    "agentic_default",
})

# Livelli di complessita' del task agentico, accettati dal classifier.
ALLOWED_COMPLEXITY: FrozenSet[str] = frozenset({"low", "medium", "high"})
