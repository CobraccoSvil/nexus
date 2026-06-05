"""Helper condiviso per caricare le API key dei provider AI dal DB Nexus.

Risolve il bug strutturale: il design Nexus prevede che le API key siano
salvate SOLO nel DB (`settings.{provider}_api_key`) e che il file `.env`
NON le contenga (commento esplicito in `.env`: "i servizi devono leggerle
dal DB"). MA i provider Python le leggevano via `os.getenv("OPENAI_API_KEY")`
che era sempre vuoto, causando "[OpenAI API key not configured]" anche con
key valida nel DB.

Questo helper centralizza la lettura: ogni provider chiama
`load_api_key("openai")` → legge `settings.openai_api_key` dal DB con
cache 60s in-process.

Fallback per backward compat: se il DB non e' raggiungibile, prova env var
`{PROVIDER}_API_KEY` (es. `OPENAI_API_KEY`).
"""
from __future__ import annotations

import logging
import os
import time
from brain.utils.db_pool import get_db_url

logger = logging.getLogger(__name__)


# Cache 60s per provider
_CACHE: dict[str, str] = {}
_CACHE_TS: dict[str, float] = {}
_TTL_S = 60.0


def _load_from_db(provider: str) -> str | None:
    """Legge `settings.{provider}_api_key` da Postgres. Ritorna None se DB
    irraggiungibile o key vuota/assente."""
    try:
        import psycopg2  # type: ignore[import]
        db_url = get_db_url()
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT value FROM settings WHERE key = %s",
                    (f"{provider}_api_key",),
                )
                row = cur.fetchone()
        if row and row[0] and str(row[0]).strip():
            return str(row[0]).strip()
        return None
    except Exception as e:
        logger.warning("api_key_loader: load DB fallito per %s (%s)", provider, e)
        return None


def load_api_key(provider: str) -> str:
    """Restituisce la API key per `provider`, letta da DB con cache 60s.
    Se il DB non e' disponibile o la key non e' configurata, fa fallback
    su env var `{PROVIDER}_API_KEY` (es. `OPENAI_API_KEY`). Ritorna stringa
    vuota se non trovata da nessuna parte (i provider gestiscono questo caso
    con il loro `[Provider API key not configured]`)."""
    now = time.time()
    try:
        from brain.utils.settings_db import get_int_setting
        _ttl = float(get_int_setting("providers.api_key_cache_ttl_seconds", 60))
    except Exception:
        _ttl = _TTL_S
    if provider in _CACHE and (now - _CACHE_TS.get(provider, 0.0)) < _ttl:
        return _CACHE[provider]
    key = _load_from_db(provider)
    if key is None:
        # Fallback env var (backward compat)
        env_name = f"{provider.upper()}_API_KEY"
        key = os.environ.get(env_name, "")
        if key:
            logger.info("api_key_loader: %s letta da env var %s (DB vuoto)", provider, env_name)
    _CACHE[provider] = key or ""
    _CACHE_TS[provider] = now
    return key or ""


def invalidate_cache(provider: str | None = None) -> None:
    """Invalida la cache (per tutti i provider o uno specifico).
    Utile dopo che l'admin aggiorna una key dall'UI."""
    global _CACHE, _CACHE_TS
    if provider is None:
        _CACHE.clear()
        _CACHE_TS.clear()
    else:
        _CACHE.pop(provider, None)
        _CACHE_TS.pop(provider, None)
