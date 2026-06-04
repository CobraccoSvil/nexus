"""Regressione per _detect_unfulfilled_intent (rilevamento intento non compiuto).

Cattura il caso reale Beauty-Book chat 7 (run gemini-2.5-pro su BookingPage.tsx):
il modello annuncia "Estrarro/Scomporro... Inizio creando la directory" e chiude
il turno senza eseguire le edit. Il guardrail route_after_executor (G1) deve
rilevare l'intento non compiuto e ri-mandare all'executor.

Il fix sostituisce la blacklist-di-frasi (fragile, cresceva a ogni verbo) con un
rilevamento morfologico: futuro 1a persona italiano (-rò) e trigger d'avvio +
gerundio. I test verificano sia gli hit attesi sia l'assenza di falsi positivi
critici (però, secondo, mondo, conclusioni).
"""

from brain.agents.nodes.helpers import _detect_unfulfilled_intent


# Casi che DEVONO far scattare il re-route (intento annunciato, non eseguito).
POSITIVI = [
    # Caso reale BookingPage: coda con "inizio creando" (gerundio).
    "Per risolvere procedero con un refactoring. Inizio creando la directory "
    "per i custom hooks.",
    # Futuro 1a persona accentato (i verbi reali del run gemini).
    "Estrarrò la logica di inizializzazione e poi scomporrò il JSX.",
    "Per prima cosa scomporrò il componente in parti piu' piccole.",
    # Verbi nuovi mai elencati in blacklist: il morfologico li copre comunque.
    "Adesso dividerò il file e sposterò gli helper in un modulo dedicato.",
    "Rifattorizzerò la funzione per ridurre la complessita'.",
    # Trigger d'avvio + gerundio.
    "Bene, sto procedendo con la creazione dei file di test.",
    "Ora generando i test per il modulo di autenticazione.",
    # Pattern storici della blacklist (non devono regredire).
    "Inizio verificando il file index.html.",
    "Let me check the configuration first.",
]

# Casi che NON devono far scattare (falsi positivi da evitare).
NEGATIVI = [
    # "però" termina in "rò" ma non e' un futuro: escluso esplicitamente.
    "Funziona correttamente, però resta un dettaglio minore da chiarire.",
    "Questo pero non risolve la causa radice del problema.",
    # Conclusioni reali (task completato): nessun annuncio futuro.
    "Ho completato il refactoring: il file e' diviso in tre componenti.",
    "Tutto funziona correttamente, ho finito il lavoro richiesto.",
    # Parole che finiscono in -ndo/-rò ma non sono gerundi/futuri d'azione.
    "Il secondo file e' gia' pronto e testato con successo.",
    "Questa soluzione copre tutto il mondo dei casi possibili.",
    # Vuoto / None.
    "",
]


def test_intento_non_compiuto_scatta_sui_positivi():
    for txt in POSITIVI:
        assert _detect_unfulfilled_intent(txt) is True, f"atteso True per: {txt!r}"


def test_nessun_falso_positivo_sui_negativi():
    for txt in NEGATIVI:
        assert _detect_unfulfilled_intent(txt) is False, f"atteso False per: {txt!r}"


def test_none_e_whitespace():
    assert _detect_unfulfilled_intent(None) is False
    assert _detect_unfulfilled_intent("   \n  ") is False


def test_caso_reale_bookingpage_coda_lunga():
    # Messaggio lungo: la valutazione e' sulla coda (ultimi 400 char), che deve
    # contenere l'annuncio per far scattare il guardrail.
    msg = (
        "Ok, ho analizzato il file BookingPage.tsx e la complessita' ciclomatica "
        "elevata (25) e' un problema. La causa principale e' troppa logica in un "
        "singolo componente. Per risolvere procedero con un refactoring in piu' "
        "passaggi: estrarro la logica in un custom hook e scomporro il JSX. "
        "Inizio creando la directory per i custom hooks."
    )
    assert _detect_unfulfilled_intent(msg) is True
