"""CRUD lato Python su nexus_subagent_definitions + nexus_subagent_runs (PR-3)."""
from __future__ import annotations

import json
import logging
import os
from typing import Any

logger = logging.getLogger(__name__)


def _conn():
    url = os.environ.get("DATABASE_URL", "")
    if not url:
        return None
    try:
        import psycopg2  # type: ignore[import-untyped]
        from psycopg2.extras import RealDictCursor  # type: ignore[import-untyped]
        return psycopg2.connect(url, cursor_factory=RealDictCursor)
    except Exception as exc:
        logger.warning("subagent_store: connessione fallita: %s", exc)
        return None


def fetch_definition(kind: str) -> dict[str, Any] | None:
    conn = _conn()
    if conn is None:
        return None
    try:
        with conn.cursor() as cur:
            cur.execute(
                """SELECT kind, description, prompt_key, tool_whitelist, model_purpose,
                          max_iterations, timeout_s, is_background, is_enabled
                   FROM nexus_subagent_definitions WHERE kind = %s AND is_enabled = true""",
                (kind,),
            )
            row = cur.fetchone()
            return dict(row) if row else None
    finally:
        conn.close()


def update_run_start(run_id: str) -> None:
    """Marca la sub-run come running e setta started_at se la colonna esistesse."""
    conn = _conn()
    if conn is None:
        return
    try:
        with conn.cursor() as cur:
            cur.execute(
                "UPDATE nexus_subagent_runs SET status = 'running' WHERE id = %s",
                (run_id,),
            )
        conn.commit()
    except Exception as exc:
        logger.warning("subagent_store.update_run_start fallita: %s", exc)
    finally:
        conn.close()


def update_run_completion(
    run_id: str,
    *,
    status: str,
    final_summary: str | None,
    artifacts: list[str],
    iterations: int,
    tokens_prompt: int,
    tokens_completion: int,
    cost_usd: float,
) -> None:
    conn = _conn()
    if conn is None:
        return
    try:
        with conn.cursor() as cur:
            cur.execute(
                """UPDATE nexus_subagent_runs SET
                       status = %s,
                       final_summary = %s,
                       artifacts = %s,
                       iterations = %s,
                       tokens_prompt = %s,
                       tokens_completion = %s,
                       cost_usd = %s,
                       completed_at = NOW()
                   WHERE id = %s""",
                (status, final_summary, list(artifacts or []), int(iterations),
                 int(tokens_prompt), int(tokens_completion), float(cost_usd), run_id),
            )
        conn.commit()
    except Exception as exc:
        logger.warning("subagent_store.update_run_completion fallita: %s", exc)
    finally:
        conn.close()


def fetch_run(run_id: str) -> dict[str, Any] | None:
    conn = _conn()
    if conn is None:
        return None
    try:
        with conn.cursor() as cur:
            cur.execute(
                """SELECT id::text, parent_run_id::text, project_id::text, kind,
                          task_description, context_blob, expected_format,
                          status, is_background, final_summary, artifacts,
                          iterations, tokens_prompt, tokens_completion, cost_usd,
                          depth, source, created_at, completed_at
                   FROM nexus_subagent_runs WHERE id = %s""",
                (run_id,),
            )
            row = cur.fetchone()
            return dict(row) if row else None
    finally:
        conn.close()
