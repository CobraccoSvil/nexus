"""Lettura sincrona delle impostazioni dalla tabella settings di Nexus.

Usato dai moduli Python che si inizializzano al momento dell'import
(provider, registry, ecc.) e non hanno accesso a un event loop async.
L'env var corrispondente resta come override di emergenza con priorita'
piu' alta: ogni funzione del modulo chiamante dovrebbe controllare prima
l'env var e ricadere qui solo se assente.

Strategia di fallback:
  1. Env var specifica (override emergenza, priorita' massima)
  2. Tabella settings nel DB (valore canonico)
  3. Default hardcoded nella chiamata get_setting(key, default)

Il modulo non importa nulla di Nexus per evitare dipendenze circolari:
usa solo psycopg2 (gia' presente nel venv brain) e os.
"""
from __future__ import annotations

import logging
import os
from typing import Optional

logger = logging.getLogger(__name__)


def _db_url() -> str:
    return os.environ.get("DATABASE_URL") or os.environ.get("POSTGRES_URL", "")


def get_setting(key: str, default: str = "") -> str:
    """Legge il valore di un'impostazione dalla tabella settings.

    Ritorna `default` se il DB non e' raggiungibile, la chiave non esiste
    o psycopg2 non e' installato. Non solleva mai eccezioni.
    """
    db_url = _db_url()
    if not db_url:
        logger.debug("settings_db: DATABASE_URL assente, uso default=%r per key=%r", default, key)
        return default
    try:
        import psycopg2
        conn = psycopg2.connect(db_url)
        try:
            with conn.cursor() as cur:
                cur.execute("SELECT value FROM settings WHERE key = %s", (key,))
                row = cur.fetchone()
                value = row[0] if row else default
                logger.debug("settings_db: key=%r value=%r (fonte=%s)",
                             key, value, "DB" if row else "default")
                return value
        finally:
            conn.close()
    except Exception as exc:
        logger.debug("settings_db.get_setting(%r) fallito: %s — uso default=%r", key, exc, default)
        return default


def get_bool_setting(key: str, default: bool = False) -> bool:
    """Legge un'impostazione booleana dalla tabella settings.

    Valori considerati True: true, 1, yes, on (case-insensitive).
    """
    raw = get_setting(key, "true" if default else "false").strip().lower()
    return raw in ("true", "1", "yes", "on")


def get_int_setting(key: str, default: int = 0) -> int:
    """Legge un'impostazione intera dalla tabella settings."""
    raw = get_setting(key, str(default)).strip()
    try:
        return int(raw)
    except ValueError:
        logger.debug("settings_db: key=%r valore=%r non e' un intero, uso default=%r", key, raw, default)
        return default


def resolve_port(key: str) -> int:
    """Risolve una porta di bind leggendola ESCLUSIVAMENTE dal DB (tabella
    settings, regola G: il DB e' l'unica fonte di verita'). Nessun default
    hardcoded e nessuna env var: se il valore non e' disponibile solleva
    RuntimeError. Coerente con `nexus_auth::resolve_port` lato Rust.

    - DB irraggiungibile: retry 5 tentativi x 5s, poi RuntimeError.
    - chiave assente / valore non valido: RuntimeError immediato (config errata
      o migrazione 0239 non applicata): meglio non partire che fare bind su una
      porta sbagliata silenziosamente.
    """
    import time

    db_url = _db_url()
    if not db_url:
        raise RuntimeError(
            f"resolve_port: DATABASE_URL/POSTGRES_URL assente, impossibile leggere settings.{key}"
        )
    import psycopg2

    for attempt in range(1, 6):
        try:
            conn = psycopg2.connect(db_url)
            try:
                with conn.cursor() as cur:
                    cur.execute("SELECT value FROM settings WHERE key = %s", (key,))
                    row = cur.fetchone()
            finally:
                conn.close()
        except psycopg2.OperationalError as exc:
            if attempt < 5:
                logger.warning(
                    "resolve_port: tentativo %d/5 lettura settings.%s fallito (%s). Retry 5s...",
                    attempt, key, exc,
                )
                time.sleep(5)
                continue
            raise RuntimeError(
                f"resolve_port: impossibile leggere settings.{key} dal DB dopo 5 tentativi: {exc}"
            ) from exc

        if row is None or not str(row[0]).strip():
            raise RuntimeError(
                f"resolve_port: settings.{key} assente nel DB. Applica la migrazione "
                f"db/migrations/0239_infrastructure_ports.sql (regola G: niente porte hardcoded)."
            )
        try:
            port = int(str(row[0]).strip())
        except ValueError as exc:
            raise RuntimeError(
                f"resolve_port: settings.{key} = {row[0]!r} non e' una porta valida."
            ) from exc
        if not 0 < port <= 65535:
            raise RuntimeError(f"resolve_port: settings.{key} = {port} fuori range 1..65535.")
        return port

    raise RuntimeError(f"resolve_port: loop di retry terminato senza esito per {key}")
