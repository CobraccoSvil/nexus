"""Test mig 0430: segnale strutturale "report con passi pendenti".

Copre la funzione PURA `detect_pending_steps_report` (helpers.py) e
l'integrazione nel punto unico `_unfulfilled_signal` (routing.py).

Esecuzione senza pytest:
    python3 -c "import brain.tests.test_pending_steps_report as t; t.run_all()"

oppure via pytest:
    pytest brain/tests/test_pending_steps_report.py -o addopts="" -q
"""
from __future__ import annotations

from brain.agents.nodes.helpers import detect_pending_steps_report
from brain.agents.nodes.routing import _unfulfilled_signal


# ─────────────────────────────────────────────────────────────────────────────
# detect_pending_steps_report — funzione pura
# ─────────────────────────────────────────────────────────────────────────────


def test_pending_steps_caso_reale_italiano() -> None:
    """Il caso reale "Continuo non riprende dopo report": etichetta
    "Prossimi passi necessari" + 2 item numerati -> True."""
    txt = (
        "Stato attuale: ho creato il file index.html e configurato la rotta.\n"
        "\n"
        "Prossimi passi necessari:\n"
        "1. Verificare il funzionamento del frontend\n"
        "2. Eseguire i test e2e\n"
    )
    assert detect_pending_steps_report(txt, min_items=2) is True


def test_pending_steps_bullet_marker() -> None:
    """Bullet "- " (non numerati) -> riconosciuti come item."""
    txt = (
        "Riepilogo del lavoro svolto.\n\n"
        "TODO:\n"
        "- Validare la migrazione DB\n"
        "- Aggiornare la documentazione\n"
        "- Lanciare il deploy locale\n"
    )
    assert detect_pending_steps_report(txt, min_items=2) is True


def test_pending_steps_inglese() -> None:
    """Etichetta inglese "Next steps" + bullet asterisco."""
    txt = (
        "I've implemented the login flow and added integration tests.\n\n"
        "Next steps:\n"
        "* Wire up the password reset endpoint\n"
        "* Add rate limiting middleware\n"
    )
    assert detect_pending_steps_report(txt, min_items=2) is True


def test_pending_steps_remaining_steps() -> None:
    """Etichetta "Remaining steps" + numerati con ")"."""
    txt = (
        "Remaining steps:\n"
        "1) review PR\n"
        "2) merge to main\n"
        "3) deploy\n"
    )
    assert detect_pending_steps_report(txt, min_items=2) is True


def test_pending_steps_da_fare() -> None:
    """Etichetta italiana corta "Da fare:" + bullet."""
    txt = (
        "Lavoro completato sul backend.\n\n"
        "Da fare:\n"
        "- Aggiornare il frontend\n"
        "- Scrivere i test\n"
    )
    assert detect_pending_steps_report(txt, min_items=2) is True


def test_no_pending_steps_solo_un_item_sotto_soglia() -> None:
    """Un solo item dopo l'etichetta (min_items=2) -> False (sotto soglia)."""
    txt = (
        "Lavoro completato.\n\n"
        "Prossimi passi:\n"
        "1. Aggiornare la doc (opzionale)\n"
    )
    assert detect_pending_steps_report(txt, min_items=2) is False


def test_no_pending_steps_risposta_compiuta() -> None:
    """Risposta conclusa senza elenco di passi pendenti -> False."""
    txt = (
        "Ho completato l'implementazione: il login funziona, i test passano e il "
        "deploy locale e' OK. Confermo intervento concluso."
    )
    assert detect_pending_steps_report(txt, min_items=2) is False


def test_no_pending_steps_testo_vuoto_o_none() -> None:
    """Input vuoto/None/whitespace -> False (mai eccezione)."""
    assert detect_pending_steps_report(None, min_items=2) is False
    assert detect_pending_steps_report("", min_items=2) is False
    assert detect_pending_steps_report("   \n\n  ", min_items=2) is False


def test_no_pending_steps_etichetta_senza_elenco() -> None:
    """Etichetta presente ma SENZA elenco numerato/puntato sotto -> False.
    Importante per non scattare su prose che cita "next steps" in passing."""
    txt = (
        "Ho discusso i next steps con il team e siamo allineati. Il lavoro e' "
        "completato per questa iterazione."
    )
    assert detect_pending_steps_report(txt, min_items=2) is False


def test_pending_steps_finestra_taglio() -> None:
    """Etichetta a inizio testo e item OLTRE la finestra di 1500 char dopo
    NON devono scattare (l'algoritmo guarda solo il blocco subito sotto)."""
    txt = (
        "Prossimi passi: vedi sotto.\n"
        + ("Lorem ipsum dolor sit amet. " * 80)  # ~2240 chars di rumore
        + "\n1. Item lontano A\n2. Item lontano B\n"
    )
    # I bullet sono fuori dalla finestra di 1500 char post-etichetta -> False.
    assert detect_pending_steps_report(txt, min_items=2) is False


def test_pending_steps_min_items_personalizzato() -> None:
    """Con min_items=3, 2 item non bastano."""
    txt = (
        "Prossimi passi:\n"
        "1. Uno\n"
        "2. Due\n"
    )
    assert detect_pending_steps_report(txt, min_items=2) is True
    assert detect_pending_steps_report(txt, min_items=3) is False


def test_pending_steps_idempotenza() -> None:
    """Funzione pura: stesso input -> stesso output, ripetibile."""
    txt = "TODO:\n- A\n- B\n- C\n"
    res1 = detect_pending_steps_report(txt, min_items=2)
    res2 = detect_pending_steps_report(txt, min_items=2)
    res3 = detect_pending_steps_report(txt, min_items=2)
    assert res1 is True and res2 is True and res3 is True


# ─────────────────────────────────────────────────────────────────────────────
# Integrazione in _unfulfilled_signal (routing.py)
# ─────────────────────────────────────────────────────────────────────────────


def test_unfulfilled_signal_intercetta_report_pending() -> None:
    """Il caso reale: closure_verdict assente, ma `result` contiene un report
    con passi pendenti -> _unfulfilled_signal deve ritornare True (cosi' il
    guard "azioni produttive + !unfulfilled" non chiude il run come finale)."""
    state = {
        "result": (
            "Stato attuale: backend e frontend integrati.\n\n"
            "Prossimi passi necessari:\n"
            "1. Verificare il deploy locale\n"
            "2. Validare i test e2e\n"
        ),
    }
    assert _unfulfilled_signal(state) is True


def test_unfulfilled_signal_judge_priority_su_pending() -> None:
    """Il verdetto del closure_judge ha priorita' sul segnale strutturale:
    se il judge dice fulfilled=True (es. il modello dichiara i passi come
    opzionali) -> _unfulfilled_signal e' False anche con elenco pending."""
    state = {
        "closure_verdict": {"fulfilled": True, "reason": "follow-up opzionali"},
        "result": (
            "Lavoro concluso.\n\nProssimi passi (opzionali):\n"
            "1. Migliorare i log\n2. Aggiungere metriche\n"
        ),
    }
    assert _unfulfilled_signal(state) is False


def test_unfulfilled_signal_judge_unfulfilled_indipendente_da_pending() -> None:
    """Se il judge dice fulfilled=False, il segnale e' True a prescindere dal
    contenuto del result (nessuna doppia decisione)."""
    state = {
        "closure_verdict": {"fulfilled": False, "reason": "rimandato"},
        "result": "Risposta normale senza elenchi.",
    }
    assert _unfulfilled_signal(state) is True


def test_unfulfilled_signal_fallback_lessicale_su_assenza_pending() -> None:
    """Niente verdict, niente pending steps -> fallback alla blacklist
    lessicale `_detect_unfulfilled_intent` (stessa semantica storica)."""
    from brain.agents.nodes.helpers import _detect_unfulfilled_intent

    txt = "Tutto a posto, ho finito il lavoro e i test passano."
    state = {"result": txt}
    assert _unfulfilled_signal(state) == _detect_unfulfilled_intent(txt)


def test_unfulfilled_signal_risposta_compiuta_no_false_positive() -> None:
    """Risposta conclusa senza elenco di passi pendenti -> False (no falsi
    positivi su risposte realmente compiute)."""
    state = {
        "result": (
            "Ho creato il file login.tsx, configurato la rotta /login e validato "
            "il deploy locale. Intervento concluso."
        ),
    }
    assert _unfulfilled_signal(state) is False


# ─────────────────────────────────────────────────────────────────────────────
# Runner manuale (convenzione brain/tests)
# ─────────────────────────────────────────────────────────────────────────────


def run_all() -> None:
    test_pending_steps_caso_reale_italiano()
    test_pending_steps_bullet_marker()
    test_pending_steps_inglese()
    test_pending_steps_remaining_steps()
    test_pending_steps_da_fare()
    test_no_pending_steps_solo_un_item_sotto_soglia()
    test_no_pending_steps_risposta_compiuta()
    test_no_pending_steps_testo_vuoto_o_none()
    test_no_pending_steps_etichetta_senza_elenco()
    test_pending_steps_finestra_taglio()
    test_pending_steps_min_items_personalizzato()
    test_pending_steps_idempotenza()
    test_unfulfilled_signal_intercetta_report_pending()
    test_unfulfilled_signal_judge_priority_su_pending()
    test_unfulfilled_signal_judge_unfulfilled_indipendente_da_pending()
    test_unfulfilled_signal_fallback_lessicale_su_assenza_pending()
    test_unfulfilled_signal_risposta_compiuta_no_false_positive()
    print("OK test_pending_steps_report (mig 0430)")


if __name__ == "__main__":
    run_all()
