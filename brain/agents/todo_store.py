"""CRUD lato Python su `nexus_agent_plans` + `nexus_agent_todos` (PR-1).

Lato brain non scrive direttamente la TODO list: quello e' compito del tool
MCP `nexus_todo_write` (handler Rust in `crates/mcp-core/src/agent_tools/todos.rs`)
chiamato dall'agente. Questo modulo serve per LEGGERE lo stato corrente del
plan e dei todos durante il loop (es. per il reminder injection in
`tool_dispatch_node`).

Tutte le funzioni sono filtrate per `project_id` quando rilevante (multi-tenant).
"""
from __future__ import annotations

import logging
import os
from typing import Any

logger = logging.getLogger(__name__)


def _get_conn():
    """Apre una connessione psycopg2 al DB Nexus.

    Ritorna None se DB non disponibile (caller gestisce graceful degrade).
    """
    database_url = os.environ.get("DATABASE_URL", "")
    if not database_url:
        return None
    try:
        import psycopg2  # type: ignore[import-untyped]
        from psycopg2.extras import RealDictCursor  # type: ignore[import-untyped]
        return psycopg2.connect(database_url, cursor_factory=RealDictCursor)
    except Exception as exc:
        logger.warning("todo_store: connessione DB fallita: %s", exc)
        return None


def fetch_plan(run_id: str) -> dict[str, Any] | None:
    """Ritorna il plan associato al run_id, o None se non esiste."""
    conn = _get_conn()
    if conn is None:
        return None
    try:
        with conn.cursor() as cur:
            cur.execute(
                """SELECT run_id::text, project_id::text, thread_id, acceptance_criteria,
                          planner_model, approved_at, score, plan_revisions, created_at
                   FROM nexus_agent_plans WHERE run_id = %s""",
                (run_id,),
            )
            row = cur.fetchone()
            return dict(row) if row else None
    except Exception as exc:
        logger.warning("todo_store.fetch_plan run_id=%s fallito: %s", run_id, exc)
        return None
    finally:
        conn.close()


def list_todos(run_id: str) -> list[dict[str, Any]]:
    """Ritorna la lista dei todos del run, ordinati per seq ascendente.

    Ogni elemento: {id, seq, content, status, priority, acceptance_criteria, verify_failures}
    """
    conn = _get_conn()
    if conn is None:
        return []
    try:
        with conn.cursor() as cur:
            cur.execute(
                """SELECT id::text, seq, content, status, priority, acceptance_criteria,
                          verify_failures, iteration_seen, updated_at
                   FROM nexus_agent_todos
                   WHERE run_id = %s
                   ORDER BY seq ASC""",
                (run_id,),
            )
            return [dict(r) for r in cur.fetchall()]
    except Exception as exc:
        logger.warning("todo_store.list_todos run_id=%s fallito: %s", run_id, exc)
        return []
    finally:
        conn.close()


def count_pending(run_id: str) -> int:
    """Conta i todos in stato non terminale (pending o in_progress)."""
    conn = _get_conn()
    if conn is None:
        return 0
    try:
        with conn.cursor() as cur:
            cur.execute(
                """SELECT COUNT(*) AS n FROM nexus_agent_todos
                   WHERE run_id = %s AND status IN ('pending','in_progress')""",
                (run_id,),
            )
            row = cur.fetchone()
            return int(row["n"]) if row else 0
    except Exception as exc:
        logger.warning("todo_store.count_pending run_id=%s fallito: %s", run_id, exc)
        return 0
    finally:
        conn.close()


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
    conn = _get_conn()
    out = {"pending": 0, "in_progress": 0, "completed": 0, "blocked": 0, "skipped": 0, "total": 0}
    if conn is None:
        return out
    try:
        with conn.cursor() as cur:
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
    finally:
        conn.close()


def all_completed(run_id: str) -> bool:
    """True se ci sono todos E tutti sono completed o skipped."""
    s = stats(run_id)
    if s["total"] == 0:
        return False
    terminal = s["completed"] + s["skipped"]
    return terminal == s["total"]


def increment_iteration_seen(run_id: str) -> None:
    """Best-effort: incrementa iteration_seen dei todos non terminali.

    Usato dal reminder injection per tracciare quante iterazioni hanno
    "visto" un todo (utile per heuristic anti-stall).
    """
    conn = _get_conn()
    if conn is None:
        return
    try:
        with conn.cursor() as cur:
            cur.execute(
                """UPDATE nexus_agent_todos
                   SET iteration_seen = iteration_seen + 1
                   WHERE run_id = %s AND status IN ('pending','in_progress')""",
                (run_id,),
            )
        conn.commit()
    except Exception as exc:
        logger.warning("todo_store.increment_iteration_seen run_id=%s fallito: %s", run_id, exc)
    finally:
        conn.close()
