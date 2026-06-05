"""Configurazione runtime per Extended Thinking (Anthropic).

Tutti i parametri vengono letti ESCLUSIVAMENTE dalla tabella `settings` del DB.
Nessuna variabile d'ambiente viene usata: ogni modifica e' applicabile
a caldo dall'interfaccia admin senza rideploy.

Cache in memoria con TTL 60 secondi: un aggiornamento admin diventa
attivo entro un minuto senza riavvio del servizio.

Chiavi DB (categoria 'agent'):
    extended_thinking_enabled       bool   default: false
    extended_thinking_budget_tokens int    default: 8000
"""
from __future__ import annotations

import logging
import threading
import time
from typing import Any

logger = logging.getLogger(__name__)

# TTL della cache locale (secondi). Ogni modifica admin diventa attiva entro questo intervallo.
_CACHE_TTL_S = 60.0

# Chiavi DB attese
_KEYS = (
    "extended_thinking_enabled",
    "extended_thinking_budget_tokens",
)

# Valori di sicurezza usati se il DB non e' raggiungibile al primo avvio.
# Conservativi: Extended Thinking disabilitato per default (fail-safe sui costi).
_SAFE_DEFAULTS: dict[str, Any] = {
    "extended_thinking_enabled": False,
    "extended_thinking_budget_tokens": 8000,
}

_lock = threading.RLock()
_cache: dict[str, Any] = dict(_SAFE_DEFAULTS)
_cache_loaded_at: float = 0.0


def _load_from_db() -> dict[str, Any]:
    """Legge i settings extended thinking dalla tabella `settings` via psycopg2.

    Restituisce i valori convertiti nel tipo corretto. Se la connessione
    fallisce, restituisce i valori gia' in cache (o i safe_defaults al primo avvio).
    """
    import os
    database_url = os.environ.get("DATABASE_URL")
    if not database_url:
        logger.warning("thinking_config: DATABASE_URL non impostato, uso safe_defaults")
        return dict(_SAFE_DEFAULTS)

    try:
        import psycopg2  # type: ignore[import-untyped]
    except ImportError:
        logger.warning("thinking_config: psycopg2 non installato, uso safe_defaults")
        return dict(_SAFE_DEFAULTS)

    try:
        conn = psycopg2.connect(database_url)
        try:
            with conn.cursor() as cur:
                keys_placeholder = ",".join(f"'{k}'" for k in _KEYS)
                cur.execute(
                    f"SELECT key, value FROM settings WHERE key IN ({keys_placeholder})"
                )
                rows = {k: v for k, v in cur.fetchall()}
        finally:
            conn.close()
    except Exception as exc:
        logger.error("thinking_config: errore lettura DB: %s", exc)
        return dict(_cache)  # mantiene valori precedenti in caso di errore transitorio

    result: dict[str, Any] = {}
    for key, safe_val in _SAFE_DEFAULTS.items():
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
                "thinking_config: valore non valido per '%s': '%s', uso default",
                key, raw,
            )
            result[key] = safe_val

    return result


def _refresh_if_stale() -> None:
    """Ricarica la cache dal DB se il TTL e' scaduto."""
    global _cache, _cache_loaded_at
    now = time.monotonic()
    with _lock:
        if now - _cache_loaded_at < _CACHE_TTL_S:
            return
        fresh = _load_from_db()
        _cache = fresh
        _cache_loaded_at = now
        logger.debug(
            "thinking_config: cache aggiornata (enabled=%s budget=%d)",
            fresh.get("extended_thinking_enabled"),
            fresh.get("extended_thinking_budget_tokens", 8000),
        )


def get() -> dict[str, Any]:
    """Restituisce la configurazione extended thinking corrente (con cache TTL 60s)."""
    _refresh_if_stale()
    with _lock:
        return dict(_cache)


# Accessori tipizzati per uso diretto dai provider

def enabled() -> bool:
    """Restituisce True se Extended Thinking e' abilitato nel DB."""
    return bool(get()["extended_thinking_enabled"])


def budget_tokens() -> int:
    """Restituisce il budget di token di ragionamento configurato nel DB."""
    return int(get()["extended_thinking_budget_tokens"])


def force_reload() -> None:
    """Invalida la cache e forza una rilettura immediata dal DB.

    Utile nei test o dopo un aggiornamento urgente dei settings.
    """
    global _cache_loaded_at
    with _lock:
        _cache_loaded_at = 0.0
