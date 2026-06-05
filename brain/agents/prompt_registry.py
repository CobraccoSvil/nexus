"""Registry Python dei prompt agenti (equivalente del registry Rust).

Legge all'avvio tutte le chiavi `agent.*` dalla tabella `nexus_prompt_templates`
e le rende disponibili ai nodi LangGraph via `get_prompt(key)`.

Dalla migrazione 0135, carica anche le **direttive condivise** dalla tabella
`nexus_shared_directives`. Queste vengono iniettate automaticamente in coda
al prompt di ogni agente che rientra nel `scope` della direttiva, eliminando
la duplicazione nei singoli template.

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
_shared_directives: list[tuple[str, str, str]] = []  # (key, content, scope)
_initialized: bool = False


def initialize(prompts: dict[str, str]) -> None:
    """Popola il registry con il dizionario chiave -> contenuto."""
    global _initialized
    with _lock:
        _prompts.update(prompts)
        _initialized = True
    logger.info("prompt_registry: %d prompt caricati (totale %d)",
                len(prompts), len(_prompts))


def _load_shared_directives(conn) -> int:  # type: ignore[no-untyped-def]
    """Carica le direttive condivise dalla tabella nexus_shared_directives.

    Ritorna il numero di direttive caricate. Se la tabella non esiste
    (migrazione non applicata), ritorna 0 senza errore.
    """
    global _shared_directives
    try:
        with conn.cursor() as cur:
            cur.execute(
                "SELECT key, content, scope FROM nexus_shared_directives "
                "WHERE is_active = TRUE "
                "ORDER BY priority ASC"
            )
            rows = cur.fetchall()
    except Exception as exc:
        # Tabella potrebbe non esistere se mig 0135 non applicata
        conn.rollback()
        logger.warning(
            "prompt_registry: nexus_shared_directives non disponibile (%s), "
            "direttive condivise disabilitate", exc
        )
        return 0
    with _lock:
        _shared_directives = [(k, c, s) for k, c, s in rows if c]
    logger.info("prompt_registry: %d direttive condivise caricate", len(_shared_directives))
    return len(_shared_directives)


def load_from_db(database_url: Optional[str] = None) -> int:
    """Popola il registry leggendo da Postgres. Ritorna il numero di chiavi.

    Se `database_url` e' None usa env `DATABASE_URL`. Se assente, ritorna 0.
    Errori di connessione sono solo loggati (non rilanciati).
    """
    url = database_url or os.environ.get("DATABASE_URL")
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
                    "WHERE key LIKE 'agent.%%' OR key LIKE 'subagent.%%'"
                )
                rows = cur.fetchall()
            _load_shared_directives(conn)
        finally:
            conn.close()
    except Exception as exc:
        logger.error("prompt_registry: errore query nexus_prompt_templates: %s", exc)
        return 0
    loaded = {k: v for k, v in rows if v}
    initialize(loaded)
    return len(loaded)


def _directives_for_key(key: str) -> str:
    """Restituisce le direttive condivise applicabili a una chiave prompt.

    Regole di scope:
    - scope='agent': applicato solo a chiavi che iniziano con 'agent.'
    - scope='system': applicato solo a chiavi che iniziano con 'system.'
    - scope='all': applicato a tutte le chiavi
    """
    with _lock:
        directives = _shared_directives
    if not directives:
        return ""
    parts: list[str] = []
    for _dkey, content, scope in directives:
        if scope == "all":
            parts.append(content)
        elif scope == "agent" and key.startswith("agent."):
            parts.append(content)
        elif scope == "system" and key.startswith("system."):
            parts.append(content)
    if not parts:
        return ""
    return "\n\n" + "\n\n".join(parts)


def get_prompt(key: str) -> str:
    """Recupera un prompt per chiave con direttive condivise, o stringa vuota."""
    with _lock:
        val = _prompts.get(key)
    if val is None:
        logger.error("AGENT PROMPT MISSING: key='%s' non in registry", key)
        return ""
    return val + _directives_for_key(key)


def get_prompt_raw(key: str) -> str:
    """Recupera il prompt senza direttive condivise (per admin UI, export)."""
    with _lock:
        val = _prompts.get(key)
    if val is None:
        return ""
    return val


def get_shared_directives() -> list[tuple[str, str, str]]:
    """Ritorna le direttive condivise caricate (key, content, scope)."""
    with _lock:
        return list(_shared_directives)


def is_initialized() -> bool:
    with _lock:
        return _initialized


def reset_for_tests() -> None:
    global _initialized, _shared_directives
    with _lock:
        _prompts.clear()
        _shared_directives = []
        _initialized = False
