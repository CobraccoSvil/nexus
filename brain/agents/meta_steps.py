"""Helper per costruire meta_step semantici emessi nel delta dei nodi LangGraph.

I meta_step sono entry strutturate aggiunte allo state in `state.meta_steps`
(annotata `add` cosi' si accumulano cross-nodo). Il generator SSE in
`brain/grpc_server/main.py::agent_run_stream` li intercetta e li ritrasmette
verso mcp-core come eventi `{"type":"meta_step", ...}`.

Schema di ciascun meta_step:
    {
        "kind": "plan" | "routing" | "clarify" | "fallback" | "reflection",
        "title": str,                  # titolo umano sintetico mostrato in UI
        "payload": dict,               # struttura specifica per kind
        "correlation_id": str | None,  # opzionale, lega a tool_use precedente
        "created_at": str (ISO8601),
    }

Feature flag: ogni `kind` può essere disabilitato tramite la tabella `settings`
con chiavi `orchestrator.meta_steps.<kind>_enabled`. Lo switch viene letto
all'avvio del processo brain (cache TTL 60s in `orchestrator_config`) ed e'
applicato qui a monte della pubblicazione.
"""
from __future__ import annotations

import logging
import os
import time
from datetime import datetime, timezone
from typing import Any

logger = logging.getLogger(__name__)

# Cache settings.meta_steps.* — TTL 60s, ricarica lazy.
_FLAG_CACHE: dict[str, bool] = {}
_FLAG_CACHE_AT: float = 0.0
_FLAG_TTL_SECS = 60.0


def _load_flags() -> dict[str, bool]:
    """Carica i flag meta_steps.* dalla tabella settings. Default: tutti True
    tranne `reflection_enabled` (default False perche' costoso)."""
    global _FLAG_CACHE, _FLAG_CACHE_AT
    now = time.monotonic()
    if _FLAG_CACHE and (now - _FLAG_CACHE_AT) < _FLAG_TTL_SECS:
        return _FLAG_CACHE
    defaults = {
        "plan_enabled": True,
        "routing_enabled": True,
        "clarify_enabled": True,
        "fallback_enabled": True,
        "reflection_enabled": False,
        "global_enabled": True,
    }
    url = os.environ.get("DATABASE_URL")
    if not url:
        _FLAG_CACHE = defaults
        _FLAG_CACHE_AT = now
        return defaults
    try:
        import psycopg2  # type: ignore[import-untyped]
        with psycopg2.connect(url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT key, value FROM settings WHERE category = 'orchestrator' "
                    "AND key LIKE 'meta_steps.%'"
                )
                for key, value in cur.fetchall():
                    short = key.replace("meta_steps.", "")
                    if short in defaults:
                        defaults[short] = str(value).lower() not in (
                            "false", "0", "off", "no",
                        )
    except Exception as exc:
        logger.debug("meta_steps._load_flags: fallback default (%s)", exc)
    _FLAG_CACHE = defaults
    _FLAG_CACHE_AT = now
    return defaults


def make(
    kind: str,
    title: str,
    payload: dict[str, Any] | None = None,
    correlation_id: str | None = None,
) -> dict[str, Any] | None:
    """Costruisce un dict meta_step pronto per essere inserito in
    `state["meta_steps"]`. Ritorna None se il kind e' disabilitato (cosi' il
    caller puo' fare `if step := meta_steps.make(...): updates["meta_steps"]
    = [step]` senza ulteriori check)."""
    flags = _load_flags()
    if not flags.get("global_enabled", True):
        return None
    flag_key = f"{kind}_enabled"
    if flag_key in flags and not flags[flag_key]:
        return None
    out: dict[str, Any] = {
        "kind": kind,
        "title": title,
        "payload": payload or {},
        "created_at": datetime.now(timezone.utc).isoformat(),
    }
    if correlation_id:
        out["correlation_id"] = correlation_id
    return out


def persist_async(run_id: str | None, step: dict[str, Any]) -> None:
    """Best-effort: inserisce il meta_step in nexus_agent_meta_steps senza
    bloccare il graph. Errori solo log. Idempotente per (run_id, created_at,
    kind) — il caller chiama una sola volta per step.

    Pensata per essere chiamata dai nodi DOPO `make()`. Se DATABASE_URL non e'
    configurato (test in-memory) e' un no-op silente.
    """
    if not run_id:
        return
    url = os.environ.get("DATABASE_URL")
    if not url:
        return
    try:
        import psycopg2  # type: ignore[import-untyped]
        import json as _json
        with psycopg2.connect(url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    """INSERT INTO nexus_agent_meta_steps
                       (run_id, kind, title, payload, correlation_id, created_at)
                       VALUES (%s, %s, %s, %s::jsonb, %s, %s)""",
                    (
                        run_id,
                        step.get("kind"),
                        step.get("title", ""),
                        _json.dumps(step.get("payload") or {}),
                        step.get("correlation_id"),
                        step.get("created_at"),
                    ),
                )
            conn.commit()
    except Exception as exc:
        logger.debug("meta_steps.persist_async: fallita (%s)", exc)
