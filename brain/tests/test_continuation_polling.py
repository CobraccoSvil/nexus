"""Test del rilevamento polling/attesa e del resoconto onesto.

Regressione del caso Beauty-Book (run 2026-06-07): l'agente in modalita' confirm
ha chiuso il turno con "Attendo qualche istante e verifico di nuovo" senza
eseguire alcun tool e senza dare un resoconto. I pattern di intenzione non
coprivano il polling (presente indicativo, non futuro/gerundio), quindi il
segnale unfulfilled non scattava.

Funzioni pure (no DB): testabili in isolamento.
"""
from __future__ import annotations

from brain.agents.nodes.helpers import (
    _detect_unfulfilled_intent,
    _detect_polling_wait,
    build_unfulfilled_report,
)


class _Msg:
    """Mini-fake di un messaggio LangChain con content a blocchi."""

    def __init__(self, msg_type: str, content):
        self.type = msg_type
        self.content = content


# ── _detect_unfulfilled_intent: ora copre il polling ────────────────────────


def test_unfulfilled_rileva_polling_attesa():
    # Il caso reale Beauty-Book.
    txt = (
        "I container non sono ancora attivi. Attendo ancora qualche secondo e "
        "ricontrollo. I container non sono ancora pronti. Attendo qualche istante "
        "e verifico di nuovo."
    )
    assert _detect_unfulfilled_intent(txt) is True


def test_unfulfilled_rileva_varianti_polling():
    for txt in (
        "Aspetto che il servizio parta e ricontrollo.",
        "Riprovo tra qualche secondo.",
        "I'll check again in a moment.",
        "Waiting for the container to be ready.",
    ):
        assert _detect_unfulfilled_intent(txt) is True, txt


def test_unfulfilled_resta_falso_su_risposta_conclusa():
    # Risposta finale legittima: nessuna intenzione/attesa pendente.
    txt = "Ho corretto il bug e i test passano. Ecco il riepilogo delle modifiche."
    assert _detect_unfulfilled_intent(txt) is False


# ── _detect_polling_wait: distingue il polling dall'azione normale ──────────


def test_polling_wait_vero_su_attesa():
    assert _detect_polling_wait("Attendo qualche istante e verifico di nuovo.") is True
    assert _detect_polling_wait("Let me check again shortly.") is True


def test_polling_wait_falso_su_intenzione_di_azione():
    # "Ora creo il file" e' intenzione d'azione, NON polling: il nudge giusto e'
    # "agisci", non "diagnostica invece di aspettare".
    assert _detect_polling_wait("Ora creo il file di configurazione.") is False
    assert _detect_polling_wait("I'll implement the endpoint next.") is False


def test_polling_wait_falso_su_vuoto():
    assert _detect_polling_wait("") is False
    assert _detect_polling_wait(None) is False


# ── build_unfulfilled_report: resoconto onesto deterministico ───────────────


def test_report_contiene_sezioni_e_azioni():
    messages = [
        _Msg("human", "Il servizio frontend-dev e' in crash-loop."),
        _Msg(
            "ai",
            [
                {"type": "tool_use", "name": "run_command", "input": {"cmd": "docker ps"}},
                {"type": "tool_use", "name": "edit_file", "input": {"path": "docker-compose.yml"}},
            ],
        ),
        _Msg(
            "ai",
            [
                {"type": "tool_use", "name": "run_command", "input": {"cmd": "docker compose up"}},
            ],
        ),
    ]
    report = build_unfulfilled_report("Attendo e verifico di nuovo.", messages)
    assert "NON e' completato" in report
    assert "Cosa ho fatto" in report
    assert "run_command" in report  # tool aggregato
    assert "docker-compose.yml" in report  # file toccato
    assert "Prossimo passo proposto" in report
    assert "diagnosticare" in report


def test_report_senza_azioni():
    report = build_unfulfilled_report("Attendo e verifico di nuovo.", [])
    assert "nessuna azione concreta" in report
    assert "Prossimo passo proposto" in report
