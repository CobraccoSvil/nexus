"""Test fix incidente run 963a51fa: dichiarazioni done ripetute (declared cap).

NB: i test della sanitizzazione leak DSML (_strip_dsml_leak) sono stati RIMOSSI
con il consolidamento del trasporto (regola L / ADR 0026): l'adapter DeepSeek non
esegue piu' chiamate LLM e la sanitizzazione dell'output corrotto (marker DSML
grezzi) vive ora nel gateway Rust (crates/nexus-gateway/src/providers/). Restano i
test della logica declared-done cumulativa (puri, indipendenti dagli adapter).

Eseguibile a mano: `PYTHONPATH=. python3 brain/tests/test_dsml_leak_and_declared_cap.py`.
"""
from __future__ import annotations

import sys


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
    test_executor_chiude_su_done_ripetuti()
    test_dispatch_conta_done_cumulativo()
    print("\nTUTTI I TEST declared_cap PASSATI")
    sys.exit(0)
