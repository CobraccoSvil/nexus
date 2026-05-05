"""Persistenza locale delle interazioni agent per apprendimento."""
from __future__ import annotations

import json
import logging
import sqlite3
from datetime import datetime, timezone
from typing import Any

logger = logging.getLogger(__name__)


class LocalLearningStorage:
    """Salva e recupera interazioni agent da SQLite per analisi e RAG."""

    def __init__(self, db_path: str) -> None:
        self.db_path = db_path
        self._init_db()

    def _connect(self) -> sqlite3.Connection:
        conn = sqlite3.connect(self.db_path, check_same_thread=False)
        conn.row_factory = sqlite3.Row
        return conn

    def _init_db(self) -> None:
        with self._connect() as conn:
            conn.executescript("""
                CREATE TABLE IF NOT EXISTS interactions (
                    id             INTEGER PRIMARY KEY AUTOINCREMENT,
                    thread_id      TEXT    NOT NULL,
                    timestamp      TEXT    NOT NULL,
                    task_type      TEXT    NOT NULL,
                    behavior_mode  TEXT    NOT NULL DEFAULT 'bilanciata',
                    user_input     TEXT    NOT NULL,
                    agent_output   TEXT    NOT NULL,
                    provider       TEXT,
                    model          TEXT,
                    latency_ms     REAL,
                    token_usage    INTEGER,
                    feedback_score REAL,
                    qdrant_id      TEXT,
                    metadata       TEXT
                );

                CREATE TABLE IF NOT EXISTS task_stats (
                    task_type      TEXT PRIMARY KEY,
                    total_count    INTEGER DEFAULT 0,
                    success_count  INTEGER DEFAULT 0,
                    avg_latency_ms REAL    DEFAULT 0.0,
                    avg_feedback   REAL    DEFAULT 0.0,
                    last_updated   TEXT    NOT NULL
                );
            """)
            conn.commit()

    def save_interaction(
        self,
        *,
        thread_id: str,
        task_type: str,
        behavior_mode: str,
        user_input: str,
        agent_output: str,
        provider: str | None = None,
        model: str | None = None,
        latency_ms: float | None = None,
        token_usage: int | None = None,
        feedback_score: float | None = None,
        qdrant_id: str | None = None,
        metadata: dict[str, Any] | None = None,
    ) -> int:
        """Salva un'interazione e aggiorna le statistiche aggregate."""
        timestamp = datetime.now(tz=timezone.utc).isoformat()
        meta_json = json.dumps(metadata) if metadata else None

        with self._connect() as conn:
            cur = conn.execute(
                """
                INSERT INTO interactions
                    (thread_id, timestamp, task_type, behavior_mode,
                     user_input, agent_output, provider, model,
                     latency_ms, token_usage, feedback_score, qdrant_id, metadata)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    thread_id, timestamp, task_type, behavior_mode,
                    user_input, agent_output, provider, model,
                    latency_ms, token_usage, feedback_score, qdrant_id, meta_json,
                ),
            )
            row_id = cur.lastrowid or 0
            conn.commit()

        self._update_stats(task_type, latency_ms)
        logger.debug("Interazione salvata: thread=%s task=%s row=%d", thread_id, task_type, row_id)
        return row_id

    def _update_stats(self, task_type: str, latency_ms: float | None) -> None:
        now = datetime.now(tz=timezone.utc).isoformat()
        with self._connect() as conn:
            conn.execute(
                """
                INSERT INTO task_stats (task_type, total_count, success_count, avg_latency_ms, last_updated)
                VALUES (?, 1, 1, ?, ?)
                ON CONFLICT(task_type) DO UPDATE SET
                    total_count    = total_count + 1,
                    success_count  = success_count + 1,
                    avg_latency_ms = (avg_latency_ms * total_count + COALESCE(excluded.avg_latency_ms, 0))
                                     / (total_count + 1),
                    last_updated   = excluded.last_updated
                """,
                (task_type, latency_ms or 0.0, now),
            )
            conn.commit()

    def update_feedback(self, thread_id: str, score: float) -> bool:
        """Aggiorna il feedback score per l'ultima interazione di un thread."""
        with self._connect() as conn:
            cur = conn.execute(
                """
                UPDATE interactions
                SET feedback_score = ?
                WHERE id = (
                    SELECT id FROM interactions
                    WHERE thread_id = ?
                    ORDER BY id DESC LIMIT 1
                )
                """,
                (score, thread_id),
            )
            conn.commit()

        if cur.rowcount == 0:
            logger.warning("Nessuna interazione trovata per thread_id=%s", thread_id)
            return False

        # Aggiorna avg_feedback in task_stats
        with self._connect() as conn:
            row = conn.execute(
                "SELECT task_type FROM interactions WHERE thread_id = ? ORDER BY id DESC LIMIT 1",
                (thread_id,),
            ).fetchone()
            if row:
                task_type = row["task_type"]
                avg = conn.execute(
                    "SELECT AVG(feedback_score) FROM interactions WHERE task_type = ? AND feedback_score IS NOT NULL",
                    (task_type,),
                ).fetchone()[0]
                conn.execute(
                    "UPDATE task_stats SET avg_feedback = ? WHERE task_type = ?",
                    (avg or 0.0, task_type),
                )
                conn.commit()
        return True

    def get_recent_interactions(self, limit: int = 10) -> list[dict[str, Any]]:
        """Recupera le interazioni più recenti."""
        with self._connect() as conn:
            rows = conn.execute(
                "SELECT * FROM interactions ORDER BY id DESC LIMIT ?",
                (limit,),
            ).fetchall()
        return [dict(r) for r in rows]

    def get_task_stats(self) -> list[dict[str, Any]]:
        """Recupera le statistiche aggregate per tipo di task."""
        with self._connect() as conn:
            rows = conn.execute("SELECT * FROM task_stats ORDER BY total_count DESC").fetchall()
        return [dict(r) for r in rows]

    def get_interactions_by_task(self, task_type: str, limit: int = 20) -> list[dict[str, Any]]:
        """Recupera interazioni filtrate per tipo di task."""
        with self._connect() as conn:
            rows = conn.execute(
                "SELECT * FROM interactions WHERE task_type = ? ORDER BY id DESC LIMIT ?",
                (task_type, limit),
            ).fetchall()
        return [dict(r) for r in rows]
