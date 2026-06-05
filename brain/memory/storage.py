"""Persistenza delle interazioni agent su PostgreSQL per apprendimento.

Sostituisce il precedente LocalLearningStorage basato su SQLite locale
(brain/nexus_memory/learning.db). Tutti i dati ora risiedono nel DB
PostgreSQL condiviso di Nexus, con backup automatico e accessibilita
da tutti i componenti del sistema.
"""
from __future__ import annotations

import json
import logging
import os
from datetime import datetime, timezone
from typing import Any
from brain.utils.db_pool import get_db_url

logger = logging.getLogger(__name__)


def _get_db_url() -> str:
    """Restituisce la connection string PostgreSQL da DATABASE_URL."""
    return os.environ.get(
        "DATABASE_URL",
        "postgresql://nexus:nexus@localhost:5433/nexus",
    )


class PostgresLearningStorage:
    """Salva e recupera interazioni agent da PostgreSQL per analisi e RAG.

    Interfaccia identica al vecchio LocalLearningStorage (SQLite) per
    garantire retrocompatibilita con tutti i call site esistenti.
    """

    def __init__(self, db_url: str | None = None) -> None:
        self._db_url = db_url or _get_db_url()

    def _connect(self):  # type: ignore[no-untyped-def]
        import psycopg2  # type: ignore[import-untyped]
        return psycopg2.connect(self._db_url)

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
        meta_json = json.dumps(metadata) if metadata else None

        with self._connect() as conn:
            with conn.cursor() as cur:
                cur.execute(
                    """
                    INSERT INTO brain_learning_interactions
                        (thread_id, task_type, behavior_mode,
                         user_input, agent_output, provider, model,
                         latency_ms, token_usage, feedback_score, qdrant_id, metadata)
                    VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s::jsonb)
                    RETURNING id
                    """,
                    (
                        thread_id, task_type, behavior_mode,
                        user_input, agent_output, provider, model,
                        latency_ms, token_usage, feedback_score, qdrant_id, meta_json,
                    ),
                )
                row = cur.fetchone()
                row_id = row[0] if row else 0
            conn.commit()

        self._update_stats(task_type, latency_ms)
        logger.debug("Interazione salvata in PostgreSQL: thread=%s task=%s row=%d", thread_id, task_type, row_id)
        return row_id

    def _update_stats(self, task_type: str, latency_ms: float | None) -> None:
        now = datetime.now(tz=timezone.utc)
        with self._connect() as conn:
            with conn.cursor() as cur:
                cur.execute(
                    """
                    INSERT INTO brain_task_stats (task_type, total_count, success_count, avg_latency_ms, last_updated)
                    VALUES (%s, 1, 1, %s, %s)
                    ON CONFLICT(task_type) DO UPDATE SET
                        total_count    = brain_task_stats.total_count + 1,
                        success_count  = brain_task_stats.success_count + 1,
                        avg_latency_ms = (brain_task_stats.avg_latency_ms * brain_task_stats.total_count
                                          + COALESCE(EXCLUDED.avg_latency_ms, 0))
                                         / (brain_task_stats.total_count + 1),
                        last_updated   = EXCLUDED.last_updated
                    """,
                    (task_type, latency_ms or 0.0, now),
                )
            conn.commit()

    def update_feedback(self, thread_id: str, score: float) -> bool:
        """Aggiorna il feedback score per l'ultima interazione di un thread."""
        with self._connect() as conn:
            with conn.cursor() as cur:
                cur.execute(
                    """
                    UPDATE brain_learning_interactions
                    SET feedback_score = %s
                    WHERE id = (
                        SELECT id FROM brain_learning_interactions
                        WHERE thread_id = %s
                        ORDER BY id DESC LIMIT 1
                    )
                    """,
                    (score, thread_id),
                )
                updated = cur.rowcount > 0
            conn.commit()

        if not updated:
            logger.warning("Nessuna interazione trovata per thread_id=%s", thread_id)
            return False

        # Aggiorna avg_feedback in brain_task_stats
        with self._connect() as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT task_type FROM brain_learning_interactions WHERE thread_id = %s ORDER BY id DESC LIMIT 1",
                    (thread_id,),
                )
                row = cur.fetchone()
                if row:
                    task_type = row[0]
                    cur.execute(
                        "SELECT AVG(feedback_score) FROM brain_learning_interactions WHERE task_type = %s AND feedback_score IS NOT NULL",
                        (task_type,),
                    )
                    avg_row = cur.fetchone()
                    avg = avg_row[0] if avg_row and avg_row[0] is not None else 0.0
                    cur.execute(
                        "UPDATE brain_task_stats SET avg_feedback = %s WHERE task_type = %s",
                        (avg, task_type),
                    )
            conn.commit()
        return True

    def get_recent_interactions(self, limit: int = 10) -> list[dict[str, Any]]:
        """Recupera le interazioni piu recenti."""
        with self._connect() as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT * FROM brain_learning_interactions ORDER BY id DESC LIMIT %s",
                    (limit,),
                )
                columns = [desc[0] for desc in cur.description] if cur.description else []
                rows = cur.fetchall()
        return [dict(zip(columns, row)) for row in rows]

    def get_task_stats(self) -> list[dict[str, Any]]:
        """Recupera le statistiche aggregate per tipo di task."""
        with self._connect() as conn:
            with conn.cursor() as cur:
                cur.execute("SELECT * FROM brain_task_stats ORDER BY total_count DESC")
                columns = [desc[0] for desc in cur.description] if cur.description else []
                rows = cur.fetchall()
        return [dict(zip(columns, row)) for row in rows]

    def get_interactions_by_task(self, task_type: str, limit: int = 20) -> list[dict[str, Any]]:
        """Recupera interazioni filtrate per tipo di task."""
        with self._connect() as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT * FROM brain_learning_interactions WHERE task_type = %s ORDER BY id DESC LIMIT %s",
                    (task_type, limit),
                )
                columns = [desc[0] for desc in cur.description] if cur.description else []
                rows = cur.fetchall()
        return [dict(zip(columns, row)) for row in rows]


# Alias retrocompatibile per non rompere import esistenti
LocalLearningStorage = PostgresLearningStorage
