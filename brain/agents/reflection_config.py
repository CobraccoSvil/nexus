"""Configurazione runtime per il sistema di self-reflection (Fase 2).

Tutti i parametri vengono letti ESCLUSIVAMENTE dalla tabella `settings` del DB.
Nessuna variabile d'ambiente viene usata: ogni modifica e' applicabile
a caldo dall'interfaccia admin senza rideploy.

Cache in memoria con TTL 60 secondi: un aggiornamento admin diventa
attivo entro un minuto senza riavvio del servizio.

Chiavi DB (categoria 'reflection'):
    reflection_enabled                  bool   default: true
    reflection_sample_rate              float  default: 0.3
    reflection_timeout_s                float  default: 10.0
    reflection_model                    str    default: claude-3-5-haiku-20241022
    reflection_reward_weight            float  default: 0.3
    reflection_reasoning_bank_min_score float  default: 0.85
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
    "reflection_enabled",
    "reflection_sample_rate",
    "reflection_timeout_s",
    "reflection_model",
    "reflection_reward_weight",
    "reflection_reasoning_bank_min_score",
)

# Valori di sicurezza usati se il DB non e' raggiungibile al primo avvio.
# Questi valori sono conservativi: disabilitano la reflection in caso di
# mancata connessione, per evitare comportamenti imprevedibili.
_SAFE_DEFAULTS: dict[str, Any] = {
    "reflection_enabled": False,               # fail-safe: disabilitato se DB irraggiungibile
    "reflection_sample_rate": 0.0,
    "reflection_timeout_s": 10.0,
    "reflection_model": "",                    # risolto da DB a runtime; vuoto = reflection disabilitata
    "reflection_reward_weight": 0.3,
    "reflection_reasoning_bank_min_score": 0.85,
}

_lock = threading.RLock()
_cache: dict[str, Any] = dict(_SAFE_DEFAULTS)
_cache_loaded_at: float = 0.0


def _load_from_db() -> dict[str, Any]:
    """Legge i settings reflection dalla tabella `settings` via psycopg2.

    Restituisce i valori convertiti nel tipo corretto. Se la connessione
    fallisce, restituisce i valori gia' in cache (o i safe_defaults al primo avvio).
    """
    import os
    database_url = os.environ.get("DATABASE_URL")
    if not database_url:
        logger.warning("reflection_config: DATABASE_URL non impostato, uso safe_defaults")
        return dict(_SAFE_DEFAULTS)

    try:
        import psycopg2  # type: ignore[import-untyped]
    except ImportError:
        logger.warning("reflection_config: psycopg2 non installato, uso safe_defaults")
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
        logger.error("reflection_config: errore lettura DB: %s", exc)
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
                "reflection_config: valore non valido per '%s': '%s', uso default",
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
            "reflection_config: cache aggiornata (enabled=%s sample_rate=%.2f)",
            fresh.get("reflection_enabled"),
            fresh.get("reflection_sample_rate"),
        )


def get() -> dict[str, Any]:
    """Restituisce la configurazione reflection corrente (con cache TTL 60s)."""
    _refresh_if_stale()
    with _lock:
        return dict(_cache)


# Accessori tipizzati per uso diretto dai nodi

def enabled() -> bool:
    return bool(get()["reflection_enabled"])


def sample_rate() -> float:
    return float(get()["reflection_sample_rate"])


def timeout_s() -> float:
    return float(get()["reflection_timeout_s"])


def model() -> str:
    return str(get()["reflection_model"])


def reward_weight() -> float:
    return float(get()["reflection_reward_weight"])


def reasoning_bank_min_score() -> float:
    return float(get()["reflection_reasoning_bank_min_score"])


def force_reload() -> None:
    """Invalida la cache e forza una rilettura immediata dal DB.

    Utile nei test o dopo un aggiornamento urgente dei settings.
    """
    global _cache_loaded_at
    with _lock:
        _cache_loaded_at = 0.0
