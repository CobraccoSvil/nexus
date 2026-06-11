"""Lettura sincrona delle impostazioni dalla tabella ``settings`` di Nexus.

Usato dai moduli Python che si inizializzano al momento dell'import
(provider, registry, ecc.) e non hanno accesso a un event loop async.

API (paritetica al lato Rust ``nexus-auth::settings`` dopo Wave 3 della
campagna de-duplicazione):

- ``get_setting(key, default)`` / ``get_bool_setting`` / ``get_int_setting``
  varianti legacy ``best-effort``: ingoiano l'errore DB e tornano al
  ``default`` passato. Mantenute per i call site storici dei provider.
- ``get_setting_checked``: propaga l'errore DB e non accetta fallback
  hardcoded (regola G + H). Preferibile per il codice NUOVO.

La connessione passa per ``brain.utils.db_pool.connect`` (punto unico DB,
regola L / ADR 0026). Niente connection string copiata qui.
"""
from __future__ import annotations

import logging
from typing import Any, Optional

from brain.utils.db_pool import DbUrlUnavailable, connect, get_db_url  # noqa: F401

logger = logging.getLogger(__name__)


def parse_typed_settings(
    rows: dict[str, str],
    defaults: dict[str, Any],
    log_prefix: str = "settings",
) -> dict[str, Any]:
    """Per ogni key in `defaults`, prende il raw da `rows` e lo coerce al tipo
    del default (bool/float/int/str). Se la conversione fallisce o il raw e'
    vuoto, ritorna il default. Punto unico (regola L / ADR 0026, S77): prima
    duplicato in `agents/reflection_config.py` e `agents/thinking_config.py`.

    Bool: 'true'/'1'/'yes' (case-insensitive). Float: float(raw). Int: int(raw).
    Altro: str(raw).strip().
    """
    result: dict[str, Any] = {}
    for key, safe_val in defaults.items():
        raw = rows.get(key, "")
        if not raw:
            result[key] = safe_val
            continue
        try:
            if isinstance(safe_val, bool):
                result[key] = raw.strip().lower() in ("true", "1", "yes")
            elif isinstance(safe_val, float):
                result[key] = float(raw.strip())
            elif isinstance(safe_val, int):
                result[key] = int(raw.strip())
            else:
                result[key] = raw.strip()
        except (ValueError, TypeError):
            logger.warning(
                "%s: valore non valido per '%s': '%s', uso default",
                log_prefix, key, raw,
            )
            result[key] = safe_val
    return result


def _read_setting_raw(key: str) -> Optional[str]:
    """Query unica della tabella ``settings``: punto di verita' della lettura
    (regola L / ADR 0026). Tutti gli altri helper di questo modulo delegano qui.

    Solleva l'eccezione DB sottostante (``DbUrlUnavailable`` / errori
    psycopg2): le varianti ``*_checked`` la propagano, le legacy la ingoiano.
    """
    with connect() as conn, conn.cursor() as cur:
        cur.execute("SELECT value FROM settings WHERE key = %s", (key,))
        row = cur.fetchone()
    return row[0] if row else None


# ── Varianti CHECKED (preferite per il codice nuovo): propagano l'errore. ────

def get_setting_checked(key: str) -> Optional[str]:
    """Legge una setting propagando l'errore DB (regola H). Valore RAW.

    Solleva ``DbUrlUnavailable`` se ``DATABASE_URL``/``POSTGRES_URL`` non sono
    impostate, oppure l'eccezione psycopg2 originale se il DB e' irraggiungibile.
    Ritorna ``None`` se la chiave non esiste.
    """
    return _read_setting_raw(key)


# ── Varianti LEGACY (best-effort, ingoiano gli errori). ─────────────────────

def get_setting(key: str, default: str = "") -> str:
    """Legge il valore di un'impostazione dalla tabella settings.

    Ritorna ``default`` se il DB non e' raggiungibile, la chiave non esiste o
    psycopg2 non e' installato. Non solleva mai eccezioni (best-effort).
    Mantenuta per i call site dei provider che si inizializzano all'import
    (non possono propagare). Per il codice NUOVO preferire ``get_setting_checked``.
    """
    try:
        raw = _read_setting_raw(key)
    except DbUrlUnavailable:
        logger.debug("settings_db: db_url assente, uso default=%r per key=%r", default, key)
        return default
    except Exception as exc:
        logger.debug("settings_db.get_setting(%r) fallito: %s — uso default=%r", key, exc, default)
        return default
    if raw is None:
        logger.debug("settings_db: key=%r non in DB, uso default=%r", key, default)
        return default
    logger.debug("settings_db: key=%r value=%r (DB)", key, raw)
    return raw


def get_bool_setting(key: str, default: bool = False) -> bool:
    """Variante booleana legacy best-effort (vedi ``get_setting``)."""
    return get_setting(key, "true" if default else "false").strip().lower() in (
        "true", "1", "yes", "on",
    )


def get_int_setting(key: str, default: int = 0) -> int:
    """Variante intera legacy best-effort (vedi ``get_setting``)."""
    raw = get_setting(key, str(default)).strip()
    try:
        return int(raw)
    except ValueError:
        logger.debug(
            "settings_db: key=%r valore=%r non e' un intero, uso default=%r",
            key, raw, default,
        )
        return default


def resolve_port(key: str) -> int:
    """Risolve una porta di bind leggendola ESCLUSIVAMENTE dal DB (tabella
    settings, regola G: il DB e' l'unica fonte di verita'). Nessun default
    hardcoded e nessuna env var: se il valore non e' disponibile solleva
    ``RuntimeError``. Coerente con ``nexus_auth::resolve_port`` lato Rust.

    - DB url assente: ``RuntimeError`` immediato.
    - DB irraggiungibile: retry 5 tentativi x 5s, poi ``RuntimeError``.
    - chiave assente / valore non valido: ``RuntimeError`` immediato (meglio non
      partire che fare bind su una porta sbagliata silenziosamente).

    La connessione passa per ``db_pool.connect`` (punto unico, regola L).
    """
    import time

    import psycopg2  # type: ignore[import]

    try:
        get_db_url()  # fail-fast con DbUrlUnavailable se non configurata
    except DbUrlUnavailable as exc:
        raise RuntimeError(
            f"resolve_port: {exc} (key={key})"
        ) from exc

    for attempt in range(1, 6):
        try:
            with connect() as conn, conn.cursor() as cur:
                cur.execute("SELECT value FROM settings WHERE key = %s", (key,))
                row = cur.fetchone()
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
