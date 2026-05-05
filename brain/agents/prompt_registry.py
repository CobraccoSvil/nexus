"""Registry Python dei prompt agenti (equivalente del registry Rust).

Legge all'avvio tutte le chiavi `agent.*` dalla tabella `nexus_prompt_templates`
e le rende disponibili ai nodi LangGraph via `get_prompt(key)`.

L'inizializzazione e' best-effort: se `DATABASE_URL` non e' impostato oppure
la query fallisce, il registry resta vuoto e `get_prompt` ritorna stringa
vuota (loggando la chiave mancante). In quel caso l'executor usera' il
system_text vuoto, che e' comportamento accettabile per smoke test.
"""
from __future__ import annotations

import logging
import os
import threading
from typing import Optional

logger = logging.getLogger(__name__)

_lock = threading.RLock()
_prompts: dict[str, str] = {}
_initialized: bool = False


def initialize(prompts: dict[str, str]) -> None:
    """Popola il registry con il dizionario chiave → contenuto."""
    global _initialized
    with _lock:
        _prompts.update(prompts)
        _initialized = True
    logger.info("prompt_registry: %d prompt caricati (totale %d)",
                len(prompts), len(_prompts))


def load_from_db(database_url: Optional[str] = None) -> int:
    """Popola il registry leggendo da Postgres. Ritorna il numero di chiavi.

    Se `database_url` e' None usa env `DATABASE_URL`. Se assente, ritorna 0.
    Errori di connessione sono solo loggati (non rilanciati).
    """
    url = database_url or os.environ.get("DATABASE_URL", "")
    if not url:
        logger.warning("prompt_registry: DATABASE_URL non impostato, skip load")
        return 0
    try:
        import psycopg2  # type: ignore[import-untyped]
    except ImportError:
        logger.warning("prompt_registry: psycopg2 non installato, skip load")
        return 0
    try:
        conn = psycopg2.connect(url)
        try:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT key, content FROM nexus_prompt_templates "
                    "WHERE key LIKE 'agent.%%'"
                )
                rows = cur.fetchall()
        finally:
            conn.close()
    except Exception as exc:
        logger.error("prompt_registry: errore query nexus_prompt_templates: %s", exc)
        return 0
    loaded = {k: v for k, v in rows if v}
    initialize(loaded)
    return len(loaded)


def get_prompt(key: str) -> str:
    """Recupera un prompt per chiave, o stringa vuota se assente."""
    with _lock:
        val = _prompts.get(key)
    if val is None:
        logger.error("AGENT PROMPT MISSING: key='%s' non in registry", key)
        return ""
    return val


def is_initialized() -> bool:
    with _lock:
        return _initialized


def reset_for_tests() -> None:
    global _initialized
    with _lock:
        _prompts.clear()
        _initialized = False
