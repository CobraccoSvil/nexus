"""Regressione del DAG scheduler: compute_ready_layer e il contratto depends_on.

Bug (2026-06-10): nexus_agent_todos.depends_on e' uuid[]. psycopg2 senza
array-uuid typecaster lo ritornava come STRINGA '{...}' invece di list[str].
compute_ready_layer faceva `for d in deps` iterando sui CARATTERI della stringa
('{','}') -> nessun match -> ready_layer SEMPRE vuoto -> il DAG parallelo non
partiva mai e il verifier (stessa logica) andava in falso "deadlock/blocked".

Fix: todo_store.list_todos casta depends_on::text[] -> list[str]. Questi test
fissano il contratto atteso da compute_ready_layer (depends_on come lista) e
dimostrano che la STRINGA lo rompe.
"""
from brain.agents.dag_scheduler import compute_ready_layer


def _todo(tid, status="pending", deps=None):
    return {"id": tid, "status": status, "depends_on": deps if deps is not None else []}


def test_fronte_parallelo_con_lista_vuota():
    # 4 todo radice (depends_on=[]) + 1 aggregatore: il fronte iniziale e' i 4.
    todos = [
        _todo("a"), _todo("b"), _todo("c"), _todo("d"),
        _todo("agg", deps=["a", "b", "c", "d"]),
    ]
    ready = compute_ready_layer(todos)
    assert {t["id"] for t in ready} == {"a", "b", "c", "d"}
    assert "agg" not in {t["id"] for t in ready}


def test_avanzamento_layer():
    # Completati i 4, l'aggregatore diventa ready.
    todos = [
        _todo("a", "completed"), _todo("b", "completed"),
        _todo("c", "completed"), _todo("d", "completed"),
        _todo("agg", deps=["a", "b", "c", "d"]),
    ]
    ready = compute_ready_layer(todos)
    assert {t["id"] for t in ready} == {"agg"}


def test_dipendenza_parziale_non_ready():
    # Aggregatore con una sola dep completata: NON ready.
    todos = [
        _todo("a", "completed"), _todo("b"),
        _todo("agg", deps=["a", "b"]),
    ]
    assert {t["id"] for t in compute_ready_layer(todos)} == {"b"}


def test_skipped_conta_come_soddisfatta():
    todos = [_todo("a", "skipped"), _todo("x", deps=["a"])]
    assert {t["id"] for t in compute_ready_layer(todos)} == {"x"}


def test_il_bug_stringa_rompe_il_fronte():
    # Documenta la causa radice: se depends_on arriva come STRINGA '{}' (psycopg2
    # senza cast), il fronte parallelo collassa a vuoto perche' `for d in '{}'`
    # itera sui caratteri. E' il motivo del cast ::text[] in list_todos.
    todos_buggy = [
        {"id": "a", "status": "pending", "depends_on": "{}"},
        {"id": "b", "status": "pending", "depends_on": "{}"},
    ]
    assert compute_ready_layer(todos_buggy) == []  # bug: nessuno ready (sbagliato)
    todos_fixed = [_todo("a"), _todo("b")]
    assert len(compute_ready_layer(todos_fixed)) == 2  # corretto con la lista
