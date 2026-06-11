"""CRUD lato Python su `nexus_agent_plans` + `nexus_agent_todos` (PR-1).

Lato brain non scrive direttamente la TODO list: quello e' compito del tool
MCP `nexus_todo_write` (handler Rust in `crates/mcp-core/src/agent_tools/todos.rs`)
chiamato dall'agente. Questo modulo serve per LEGGERE lo stato corrente del
plan e dei todos durante il loop (es. per il reminder injection in
`tool_dispatch_node`).

Tutte le funzioni sono filtrate per `project_id` quando rilevante (multi-tenant).
"""
from __future__ import annotations

import contextlib
import logging
from typing import Any, Iterator

logger = logging.getLogger(__name__)


@contextlib.contextmanager
def _cursor() -> Iterator[Any]:
    """Cursor RealDict su connessione prestata dal pool condiviso.

    Delega a ``brain.utils.db_pool.connect`` (punto unico DB, regola L):
    niente piu' connessione TCP nuova per ogni lettura. Le eccezioni
    (incluse ``DbUrlUnavailable``) risalgono al caller, che degrada graceful.
    """
    from psycopg2.extras import RealDictCursor  # type: ignore[import-untyped]

    from brain.utils.db_pool import connect

    with connect(cursor_factory=RealDictCursor) as conn, conn.cursor() as cur:
        yield cur


def fetch_plan(run_id: str) -> dict[str, Any] | None:
    """Ritorna il plan associato al run_id, o None se non esiste."""
    try:
        with _cursor() as cur:
            cur.execute(
                """SELECT run_id::text, project_id::text, thread_id, acceptance_criteria,
                          planner_model, approved_at, score, plan_revisions, created_at,
                          user_intent, behavior_mode
                   FROM nexus_agent_plans WHERE run_id = %s""",
                (run_id,),
            )
            row = cur.fetchone()
            return dict(row) if row else None
    except Exception as exc:
        logger.warning("todo_store.fetch_plan run_id=%s fallito: %s", run_id, exc)
        return None


def list_todos(run_id: str) -> list[dict[str, Any]]:
    """Ritorna la lista dei todos del run, ordinati per seq ascendente.

    Ogni elemento: {id, seq, content, status, priority, acceptance_criteria, verify_failures}
    """
    try:
        with _cursor() as cur:
            cur.execute(
                # depends_on e' uuid[]: psycopg2 senza array-uuid typecaster lo
                # ritorna come STRINGA '{...}' invece di lista. Il cast a text[]
                # forza il parsing nativo psycopg2 -> list[str] di UUID. Senza
                # questo, compute_ready_layer/get_next_executable_todo iteravano
                # sui caratteri della stringa ('{','}') -> nessun match -> il DAG
                # parallelo non partiva mai e il verifier andava in falso deadlock.
                # id::text allinea il tipo per il confronto str(dep) in {id}.
                """SELECT id::text, seq, content, status, priority, acceptance_criteria,
                          verify_failures, iteration_seen, updated_at,
                          depends_on::text[] AS depends_on, node_key, dag_layer
                   FROM nexus_agent_todos
                   WHERE run_id = %s
                   ORDER BY seq ASC""",
                (run_id,),
            )
            return [dict(r) for r in cur.fetchall()]
    except Exception as exc:
        logger.warning("todo_store.list_todos run_id=%s fallito: %s", run_id, exc)
        return []


def active_todo(run_id: str) -> dict[str, Any] | None:
    """Ritorna il todo "attivo" (in_progress se presente, altrimenti primo pending)."""
    todos = list_todos(run_id)
    if not todos:
        return None
    for t in todos:
        if t["status"] == "in_progress":
            return t
    for t in todos:
        if t["status"] == "pending":
            return t
    return None


def stats(run_id: str) -> dict[str, int]:
    """Conteggio per status: utile per il PlanInspector e per logging."""
    out = {"pending": 0, "in_progress": 0, "completed": 0, "blocked": 0, "skipped": 0, "total": 0}
    try:
        with _cursor() as cur:
            cur.execute(
                """SELECT status, COUNT(*) AS n FROM nexus_agent_todos
                   WHERE run_id = %s GROUP BY status""",
                (run_id,),
            )
            for r in cur.fetchall():
                out[r["status"]] = int(r["n"])
                out["total"] += int(r["n"])
            return out
    except Exception as exc:
        logger.warning("todo_store.stats run_id=%s fallito: %s", run_id, exc)
        return out


def increment_iteration_seen(run_id: str) -> None:
    """Best-effort: incrementa iteration_seen dei todos non terminali.

    Usato dal reminder injection per tracciare quante iterazioni hanno
    "visto" un todo (utile per heuristic anti-stall).
    """
    try:
        with _cursor() as cur:
            cur.execute(
                """UPDATE nexus_agent_todos
                   SET iteration_seen = iteration_seen + 1
                   WHERE run_id = %s AND status IN ('pending','in_progress')""",
                (run_id,),
            )
    except Exception as exc:
        logger.warning("todo_store.increment_iteration_seen run_id=%s fallito: %s", run_id, exc)
