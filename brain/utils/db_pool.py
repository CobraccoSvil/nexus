"""Connessione DB Python: punto unico (regola L / ADR 0026 + regola G).

Prima questa logica era duplicata in ~30 file Python (psycopg2.connect con
connection string copiata, alcuni con default hardcoded "postgres://nexus:nexus@
localhost:5433/nexus" che violavano la regola G del CLAUDE.md). Ora la
connection URL e i contesti di connessione vivono qui.

Per ora questo modulo espone helper sincroni (psycopg2) coerenti con l'uso
attuale del brain. Un pool vero (psycopg_pool.ConnectionPool) si aggiungera'
solo dopo aver convertito tutti i call site (vedi guard Wave 6a in
scripts/check-single-source.sh): introdurre il pool prima della convergenza
totale produrrebbe due API attive in parallelo, cioe' altra duplicazione.

Niente default hardcoded: se DATABASE_URL/POSTGRES_URL non sono impostate, le
funzioni sollevano `DbUrlUnavailable` (regola G: fail visibile, mai fallback
nascosto verso un DB sbagliato).
"""
from __future__ import annotations

import contextlib
import os
from typing import Any, Iterator


class DbUrlUnavailable(RuntimeError):
    """Sollevata quando DATABASE_URL/POSTGRES_URL non sono configurate.

    Non si fallisce con un default hardcoded (regola G): un fallback nascosto
    farebbe scrivere/leggere su un DB sbagliato senza che l'admin se ne accorga.
    """


def get_db_url() -> str:
    """Ritorna la connection URL del DB Nexus. Solleva ``DbUrlUnavailable`` se
    nessuna delle env var canoniche e' impostata (regola G)."""
    url = os.environ.get("DATABASE_URL") or os.environ.get("POSTGRES_URL")
    if not url:
        raise DbUrlUnavailable(
            "DATABASE_URL/POSTGRES_URL non impostate: impossibile connettersi al "
            "DB Nexus. Niente default hardcoded (regola G del CLAUDE.md)."
        )
    return url


@contextlib.contextmanager
def connect(**kwargs: Any) -> Iterator["psycopg2.extensions.connection"]:  # type: ignore[name-defined]
    """Context manager che apre/chiude una connessione psycopg2 sul DB Nexus.

    Uso::

        from brain.utils.db_pool import connect
        from psycopg2.extras import RealDictCursor

        with connect() as conn, conn.cursor() as cur:
            cur.execute("SELECT 1")

        # Kwargs psycopg2 supportati (es. cursor_factory)
        with connect(cursor_factory=RealDictCursor) as conn, conn.cursor() as cur:
            ...

    Solleva ``DbUrlUnavailable`` se la URL non e' configurata.
    """
    import psycopg2  # type: ignore[import]

    conn = psycopg2.connect(get_db_url(), **kwargs)
    try:
        yield conn
    finally:
        conn.close()
