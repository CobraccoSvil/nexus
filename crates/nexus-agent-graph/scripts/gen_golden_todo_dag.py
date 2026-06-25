#!/usr/bin/env python3
"""Genera /tmp/golden_todo_dag.json: parita' 1:1 della logica DAG dei todo.

Funzioni coperte (PUNTO UNICO Rust = crates/nexus-agent-graph/src/decisions/
dag_scheduler.rs):
  - pick_next_todo      <- brain/agents/verifier_node.py:508-534 (_pick_next_todo)
  - descendants         <- brain/agents/dag_scheduler.py:76-90  (_descendants)
  - compute_ready_layer <- brain/agents/dag_scheduler.py:38-52  (compute_ready_layer)

Strategia: lo script PROVA a importare le funzioni REALI dal brain (sono pure,
nessun I/O); se l'import fallisce (dipendenze del modulo non disponibili in CI),
ricade su una replica BYTE-FEDELE delle stesse funzioni (COPIA 1:1 dalle righe
indicate, NON re-implementazione creativa). In entrambi i casi l'output e' lo
stesso: il golden e' l'oracolo del Python.

Contratto depends_on (bug 2026-06-10): depends_on e' SEMPRE una LISTA (cast
::text[] in todo_store.list_todos). I casi qui usano liste; il caso degenere
"stringa" NON e' incluso perche' in Rust e' impossibile per costruzione (Vec).

Casi (>=25): lineare, diamante (A->B,C; B,C->D), ciclo degenere, deadlock,
deps miste completed/skipped/pending, topological ON vs OFF, nessun pending.

Uso:
    python3 crates/nexus-agent-graph/scripts/gen_golden_todo_dag.py
    (scrive /tmp/golden_todo_dag.json)
"""
import json

# ── Tentativo di import delle funzioni REALI del brain (pure, no I/O) ─────────
_REAL = False
try:  # pragma: no cover - dipende dall'ambiente
    import os
    import sys

    _here = os.path.dirname(os.path.abspath(__file__))
    _root = os.path.abspath(os.path.join(_here, "..", "..", ".."))
    if _root not in sys.path:
        sys.path.insert(0, _root)
    from brain.agents.dag_scheduler import (  # type: ignore
        _descendants as _real_descendants,
        compute_ready_layer as _real_ready_layer,
    )
    from brain.agents.verifier_node import _pick_next_todo as _real_pick_next  # type: ignore

    _REAL = True
except Exception:  # noqa: BLE001 - fallback alla replica byte-fedele
    _REAL = False


# ── Replica BYTE-FEDELE (COPIA 1:1 dal Python) ────────────────────────────────

def _ready_layer_copy(todos):
    """dag_scheduler.py:38-52 (copia 1:1)."""
    done = {str(t.get("id")) for t in todos if t.get("status") in ("completed", "skipped")}
    ready = []
    for t in todos:
        if t.get("status") != "pending":
            continue
        deps = t.get("depends_on") or []
        if all(str(d) in done for d in deps):
            ready.append(t)
    return ready


def _descendants_copy(todo_id, todos):
    """dag_scheduler.py:76-90 (copia 1:1)."""
    children = {}
    for t in todos:
        for d in t.get("depends_on") or []:
            children.setdefault(str(d), []).append(str(t.get("id")))
    out = set()
    stack = [todo_id]
    while stack:
        cur = stack.pop()
        for c in children.get(cur, []):
            if c not in out:
                out.add(c)
                stack.append(c)
    return out


def _pick_next_copy(todos, cfg):
    """verifier_node.py:508-534 (copia 1:1)."""
    pending = [t for t in todos if t.get("status") == "pending"]
    if not pending:
        return None
    dag_enabled = bool(cfg.get("dag_topological_enabled"))
    has_deps = any(t.get("depends_on") for t in todos)
    if not dag_enabled or not has_deps:
        return pending[0]
    done = {str(t.get("id")) for t in todos if t.get("status") in ("completed", "skipped")}
    for t in pending:
        deps = t.get("depends_on") or []
        if all(str(d) in done for d in deps):
            return t
    return pending[0]


# ── Dispatcher: reale se importabile, altrimenti copia ────────────────────────

def ready_layer(todos):
    return _real_ready_layer(todos) if _REAL else _ready_layer_copy(todos)


def descendants(todo_id, todos):
    return _real_descendants(todo_id, todos) if _REAL else _descendants_copy(todo_id, todos)


def pick_next(todos, dag_enabled):
    cfg = {"dag_topological_enabled": dag_enabled}
    return (_real_pick_next(todos, cfg) if _REAL else _pick_next_copy(todos, cfg))


# ── Helper costruzione todo ───────────────────────────────────────────────────

def _t(tid, status="pending", deps=None, seq=None):
    return {"id": tid, "status": status, "depends_on": deps if deps is not None else [], "seq": seq}


def main() -> None:
    cases = []

    def add(case_id, function, inp, out):
        cases.append({"case_id": case_id, "function": function, "input": inp, "output": out})

    # ── Grafi riusati ─────────────────────────────────────────────────────────
    lineare = [_t("a", "pending"), _t("b", "pending", ["a"]), _t("c", "pending", ["b"])]
    lineare_a_done = [_t("a", "completed"), _t("b", "pending", ["a"]), _t("c", "pending", ["b"])]
    diamante = [
        _t("a", "completed"), _t("b", "pending", ["a"]), _t("c", "pending", ["a"]),
        _t("d", "pending", ["b", "c"]),
    ]
    diamante_pendente = [
        _t("a", "pending"), _t("b", "pending", ["a"]), _t("c", "pending", ["a"]),
        _t("d", "pending", ["b", "c"]),
    ]
    ciclo = [_t("a", "pending", ["b"]), _t("b", "pending", ["a"])]
    deadlock = [_t("a", "blocked"), _t("b", "pending", ["a"]), _t("c", "pending", ["a"])]
    miste = [
        _t("a", "completed"), _t("b", "skipped"), _t("c", "pending", ["a", "b"]),
        _t("d", "pending", ["a", "x"]),  # x mancante -> non soddisfatta
    ]
    indipendenti = [_t("a", "pending"), _t("b", "pending"), _t("c", "pending")]
    tutti_terminali = [_t("a", "completed"), _t("b", "skipped"), _t("c", "blocked")]

    # ── compute_ready_layer ───────────────────────────────────────────────────
    def ids(rl):
        return [t["id"] for t in rl]

    for cid, todos in [
        ("rl_lineare", lineare),
        ("rl_lineare_a_done", lineare_a_done),
        ("rl_diamante", diamante),
        ("rl_diamante_pendente", diamante_pendente),
        ("rl_deadlock", deadlock),
        ("rl_miste", miste),
        ("rl_indipendenti", indipendenti),
        ("rl_tutti_terminali", tutti_terminali),
        ("rl_ciclo", ciclo),
    ]:
        add(cid, "compute_ready_layer", {"todos": todos}, ids(ready_layer(todos)))

    # ── descendants (set -> lista ordinata per confronto deterministico) ──────
    def dset(todo_id, todos):
        return sorted(descendants(todo_id, todos))

    for cid, tid, todos in [
        ("desc_lineare_a", "a", lineare),
        ("desc_lineare_c", "c", lineare),    # foglia -> vuoto
        ("desc_diamante_a", "a", diamante),  # {b,c,d}
        ("desc_diamante_b", "b", diamante),  # {d}
        ("desc_ciclo_a", "a", ciclo),        # {a,b} senza loop infinito
        ("desc_deadlock_a", "a", deadlock),  # {b,c}
        ("desc_inesistente", "zz", lineare), # id assente -> vuoto
    ]:
        add(cid, "descendants", {"todos": todos, "id": tid}, dset(tid, todos))

    # ── pick_next_todo: topological ON vs OFF, nessun pending, deadlock ───────
    def pid(todos, dag):
        sel = pick_next(todos, dag)
        return sel["id"] if sel is not None else None

    for cid, todos, dag in [
        ("pn_off_lineare", lineare, False),              # primo pending: a
        ("pn_on_lineare", lineare, True),                # a (deps vuote)
        ("pn_off_a_done", lineare_a_done, False),        # primo pending: b
        ("pn_on_a_done", lineare_a_done, True),          # b (dep a soddisfatta)
        ("pn_on_diamante", diamante, True),              # b (a completed)
        ("pn_off_diamante_pend", diamante_pendente, False),  # primo pending: a
        ("pn_on_diamante_pend", diamante_pendente, True),    # a (deps vuote)
        ("pn_on_deadlock", deadlock, True),              # fallback: b (a blocked)
        ("pn_off_deadlock", deadlock, False),            # b
        ("pn_on_miste", miste, True),                    # c (a+b soddisfatte)
        ("pn_off_miste", miste, False),                  # c (primo pending)
        ("pn_nessun_pending", tutti_terminali, True),    # None
        ("pn_nessun_pending_off", tutti_terminali, False),  # None
        ("pn_on_indip", indipendenti, True),             # a (nessuna dep)
        ("pn_on_ciclo", ciclo, True),                    # fallback: a
    ]:
        add(cid, "pick_next_todo", {"todos": todos, "dag_topological_enabled": dag},
            pid(todos, dag))

    out_path = "/tmp/golden_todo_dag.json"
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(cases, f, ensure_ascii=False, indent=2)
    src = "funzioni REALI del brain" if _REAL else "replica byte-fedele"
    print(f"golden todo_dag: {len(cases)} casi scritti in {out_path} ({src})")


if __name__ == "__main__":
    main()
