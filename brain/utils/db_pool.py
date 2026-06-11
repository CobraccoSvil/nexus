"""Connessione DB Python: punto unico (regola L / ADR 0026 + regola G).

Prima questa logica era duplicata in ~30 file Python (psycopg2.connect con
connection string copiata, alcuni con default hardcoded "postgres://nexus:nexus@
localhost:5433/nexus" che violavano la regola G del CLAUDE.md). Ora la
connection URL e i contesti di connessione vivono qui.

Da Wave 5 (perf) il context manager ``connect()`` e' servito da un
``psycopg2.pool.ThreadedConnectionPool`` inizializzato lazy alla prima
richiesta: niente piu' handshake TCP+auth per OGNI lettura (baseline misurata:
~14 ms a chiamata solo di connect/close). Le connessioni vengono prese con
``getconn`` e restituite con ``putconn`` nello stesso context manager;
su eccezione la connessione viene scartata (``putconn(close=True)``) cosi'
una connessione rotta non rientra mai nel pool.

Semantica transazionale: su uscita pulita dal blocco ``with`` viene eseguito
``conn.commit()`` (stessa semantica di ``with psycopg2.connect(...) as conn``,
che committava a fine blocco); su eccezione ``rollback`` + scarto. I commit
espliciti nei call site restano validi (commit doppio = no-op).

Niente default hardcoded: se DATABASE_URL/POSTGRES_URL non sono impostate, le
funzioni sollevano `DbUrlUnavailable` (regola G: fail visibile, mai fallback
nascosto verso un DB sbagliato).
"""
from __future__ import annotations

import contextlib
import os
import threading
from typing import Any, Iterator

# Dimensioni del pool: costanti di modulo come bootstrap infrastrutturale
# (stessa categoria di DATABASE_URL: la regola G ammette il bootstrap infra).
# Il tuning fine (minconn/maxconn da `settings`) arrivera' quando il pool
# sara' assestato: leggerle ORA da settings creerebbe una circolarita'
# (settings_db legge il DB attraverso questo stesso pool).
_POOL_MIN_CONN = 1
_POOL_MAX_CONN = 8
# Timeout handshake (secondi) per le NUOVE connessioni create dal pool: senza,
# un DB irraggiungibile bloccherebbe il chiamante fino al timeout TCP di sistema.
_POOL_CONNECT_TIMEOUT = 5

_pool: Any = None
_pool_dsn: str | None = None
_pool_lock = threading.Lock()


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


def _get_pool() -> Any:
    """Inizializza (lazy, double-checked lock) e ritorna il pool condiviso.

    Il pool e' legato al DSN con cui e' stato creato: se ``DATABASE_URL``
    cambia a runtime (tipico nei test; in produzione non accade) il pool
    vecchio viene chiuso e ricreato sul nuovo DSN — altrimenti l'env var
    diventerebbe inerte dopo la prima connessione.

    Solleva ``DbUrlUnavailable`` se la URL non e' configurata: il pool non
    viene creato e il prossimo tentativo riprova da zero.
    """
    global _pool, _pool_dsn
    dsn = get_db_url()
    if _pool is None or dsn != _pool_dsn:
        with _pool_lock:
            if _pool is None or dsn != _pool_dsn:
                if _pool is not None:
                    with contextlib.suppress(Exception):
                        _pool.closeall()
                    _pool = None
                from psycopg2.pool import ThreadedConnectionPool  # type: ignore[import]

                _pool = ThreadedConnectionPool(
                    _POOL_MIN_CONN,
                    _POOL_MAX_CONN,
                    dsn,
                    connect_timeout=_POOL_CONNECT_TIMEOUT,
                )
                _pool_dsn = dsn
    return _pool


@contextlib.contextmanager
def connect(cursor_factory: Any = None) -> Iterator["psycopg2.extensions.connection"]:  # type: ignore[name-defined]
    """Context manager che presta una connessione psycopg2 dal pool condiviso.

    Uso::

        from brain.utils.db_pool import connect
        from psycopg2.extras import RealDictCursor

        with connect() as conn, conn.cursor() as cur:
            cur.execute("SELECT 1")

        # cursor_factory applicata ai cursor aperti dentro il blocco
        with connect(cursor_factory=RealDictCursor) as conn, conn.cursor() as cur:
            ...

    Garanzie:

    - ``getconn``/``putconn`` avvengono entrambi qui (try/finally): la
      connessione torna al pool anche se il blocco solleva.
    - Uscita pulita -> ``commit()``; eccezione -> ``rollback()`` e la
      connessione viene SCARTATA (``putconn(close=True)``), mai riciclata rotta.
    - ``cursor_factory`` e' applicata alla connessione per la durata del blocco
      e azzerata prima della restituzione al pool.

    Solleva ``DbUrlUnavailable`` se la URL non e' configurata.
    """
    pool = _get_pool()
    conn = pool.getconn()
    broken = False
    try:
        if cursor_factory is not None:
            conn.cursor_factory = cursor_factory
        yield conn
        conn.commit()
    except Exception:
        broken = True
        with contextlib.suppress(Exception):
            conn.rollback()
        raise
    finally:
        with contextlib.suppress(Exception):
            conn.cursor_factory = None
        with contextlib.suppress(Exception):
            pool.putconn(conn, close=broken)


@contextlib.contextmanager
def connect_external(
    dsn: str,
    cursor_factory: Any = None,
    connect_timeout: int | None = None,
) -> Iterator["psycopg2.extensions.connection"]:  # type: ignore[name-defined]
    """Connessione one-shot a un DSN arbitrario (DB applicativi di progetto).

    NON passa dal pool: il pool e' dedicato al DB Nexus (``get_db_url()``).
    Serve ai check che interrogano i Postgres delle app utente (es.
    ``criteria_runner._check_db_query`` con ``spec.connection_string``).
    Apre e chiude la connessione nel context manager (chiusura senza commit:
    semantica identica al psycopg2.connect+close storico).
    """
    import psycopg2  # type: ignore[import]

    kwargs: dict[str, Any] = {}
    if cursor_factory is not None:
        kwargs["cursor_factory"] = cursor_factory
    if connect_timeout is not None:
        kwargs["connect_timeout"] = int(connect_timeout)
    conn = psycopg2.connect(dsn, **kwargs)
    try:
        yield conn
    finally:
        conn.close()
