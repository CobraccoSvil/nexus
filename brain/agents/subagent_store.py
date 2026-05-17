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


def fetch_definition(kind: str, project_root: str | None = None) -> dict[str, Any] | None:
    """Carica la definition di un kind di sub-agent.

    PR-3 Cursor pattern: se `project_root` fornito e contiene
    `.nexus/agents/<kind>.md`, il file YAML override SOSTITUISCE la definition
    DB centralizzata (solo per il progetto attivo). La sorgente effettiva e'
    annotata in `definition["source"]` = "db" | "project_override".
    """
    # 1. Carica baseline da DB.
    db_def: dict[str, Any] | None = None
    conn = _conn()
    if conn is not None:
        try:
            with conn.cursor() as cur:
                cur.execute(
                    """SELECT kind, description, prompt_key, tool_whitelist, model_purpose,
                              max_iterations, timeout_s, is_background, is_enabled
                       FROM nexus_subagent_definitions WHERE kind = %s AND is_enabled = true""",
                    (kind,),
                )
                row = cur.fetchone()
                db_def = dict(row) if row else None
        finally:
            conn.close()

    # 2. Carica eventuale override progetto (se abilitato).
    project_override: dict[str, Any] | None = None
    if project_root:
        try:
            from . import orchestrator_config, subagent_yaml_loader  # local import (cicli)
            cfg = orchestrator_config.get()
            if cfg.get("subagent_project_override_enabled", True):
                overrides = subagent_yaml_loader.load_project_overrides(project_root)
                project_override = overrides.get(kind)
        except Exception as exc:
            logger.debug("fetch_definition: project_override skip per kind=%s: %s", kind, exc)

    # 3. Merge: project_override prevale, fallback su db_def per i campi mancanti.
    if project_override is not None:
        merged: dict[str, Any] = dict(db_def or {})
        merged.update({k: v for k, v in project_override.items() if v is not None})
        merged.setdefault("kind", kind)
        merged.setdefault("source", "project_override")
        merged.setdefault("is_enabled", True)
        return merged
    if db_def is not None:
        db_def.setdefault("source", "db")
        return db_def
    return None


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


def list_enabled_kinds(project_root: str | None = None) -> list[dict[str, Any]]:
    """Ritorna la lista dei kind abilitati con `(kind, description, is_background)`.

    Usato dal main agent per il blocco `<available_subagents>` (auto-delegation).
    Include eventuali override progetto: se in `.nexus/agents/X.md` esiste un
    kind non in DB, viene aggiunto al risultato.
    """
    out: list[dict[str, Any]] = []
    seen: set[str] = set()
    conn = _conn()
    if conn is not None:
        try:
            with conn.cursor() as cur:
                cur.execute(
                    """SELECT kind, description, is_background
                       FROM nexus_subagent_definitions WHERE is_enabled = true ORDER BY kind"""
                )
                for r in cur.fetchall():
                    d = dict(r)
                    out.append(d)
                    seen.add(d["kind"])
        finally:
            conn.close()
    if project_root:
        try:
            from . import orchestrator_config, subagent_yaml_loader
            cfg = orchestrator_config.get()
            if cfg.get("subagent_project_override_enabled", True):
                for kind, ov in subagent_yaml_loader.load_project_overrides(project_root).items():
                    if kind in seen:
                        # Aggiorna description se override la fornisce.
                        for item in out:
                            if item.get("kind") == kind and ov.get("description"):
                                item["description"] = ov["description"]
                    else:
                        out.append({
                            "kind": kind,
                            "description": ov.get("description", "(custom project sub-agent)"),
                            "is_background": bool(ov.get("is_background", False)),
                        })
        except Exception as exc:
            logger.debug("list_enabled_kinds: project_override skip: %s", exc)
    return out


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
