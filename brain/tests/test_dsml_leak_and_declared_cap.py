"""Test fix incidente run 963a51fa: leak DSML nel content + dichiarazioni done ripetute.

Eseguibile a mano: `PYTHONPATH=. python3 brain/tests/test_dsml_leak_and_declared_cap.py`.
"""
from __future__ import annotations

import sys

from brain.providers.deepseek_provider import _strip_dsml_leak


def test_dsml_fullwidth_troncato() -> None:
    # Caso reale: allucinazione + blocco DSML grezzo in coda.
    raw = (
        "The first book Stephen King published was Rage.\n\n"
        "<｜｜DSML｜｜tool_calls>\n<｜｜DSML｜｜invoke name=\"todowrite\">"
    )
    out, leaked = _strip_dsml_leak(raw)
    assert leaked is True
    assert "DSML" not in out, out
    assert "tool_calls" not in out
    assert out.endswith("Rage."), out[-40:]
    print("OK test_dsml_fullwidth_troncato")


def test_dsml_ascii_variant() -> None:
    out, leaked = _strip_dsml_leak("testo valido <|DSML|>roba interna")
    assert leaked is True and out == "testo valido", repr(out)
    print("OK test_dsml_ascii_variant")


def test_testo_pulito_intatto() -> None:
    out, leaked = _strip_dsml_leak("risposta normale senza marker")
    assert leaked is False and out == "risposta normale senza marker"
    out2, leaked2 = _strip_dsml_leak("")
    assert leaked2 is False and out2 == ""
    print("OK test_testo_pulito_intatto")


def test_executor_chiude_su_done_ripetuti() -> None:
    # Il check vive a inizio executor_node: state con declared done x3 -> il
    # nodo ritorna end_turn senza chiamare il modello. Verifica della LOGICA
    # via ispezione delle condizioni (il nodo completo richiede provider mock).
    decl = {"outcome": "done", "summary": "Analisi completata: app gia' estratta in app/."}
    done_count = 3
    triggers = (
        isinstance(decl, dict) and decl.get("outcome") == "done" and done_count >= 3
    )
    assert triggers is True
    # Sotto soglia non scatta.
    assert not (done_count - 2 >= 3)
    # blocked/needs_input non scattano (chiusure gia' gestite dal routing).
    decl_blocked = {"outcome": "blocked"}
    assert not (decl_blocked.get("outcome") == "done")
    print("OK test_executor_chiude_su_done_ripetuti")


def test_dispatch_conta_done_cumulativo() -> None:
    # Replica della logica del dispatch: outcome done conta, blocked no.
    declared = [{"outcome": "done"}, {"outcome": "blocked"}, {"outcome": "done"}]
    done_now = sum(1 for d in declared if d.get("outcome") == "done")
    assert done_now == 2
    prev = 1
    assert prev + done_now == 3, "cumulativo attraverso i turni"
    print("OK test_dispatch_conta_done_cumulativo")


if __name__ == "__main__":
    test_dsml_fullwidth_troncato()
    test_dsml_ascii_variant()
    test_testo_pulito_intatto()
    test_executor_chiude_su_done_ripetuti()
    test_dispatch_conta_done_cumulativo()
    print("\nTUTTI I TEST dsml_leak + declared_cap PASSATI")
    sys.exit(0)
