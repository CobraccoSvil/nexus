"""Helper condiviso per caricare la lista modelli supportati per provider dal DB.

Sostituisce gli array hardcoded `list_models()` in tutti i provider cloud
(anthropic, openai, google, mistral, deepseek).

Pattern: ogni provider chiama `load_provider_catalog(provider_name)` in
`list_models()`. Cache 60s in-process per evitare query a ogni call.

Sorgente DB: tabella `ai_price_catalog` (provider, model, capabilities,
is_enabled). Quando un modello viene aggiunto/rimosso/deprecato, basta
UPDATE/INSERT lì — niente patch + redeploy.

**Niente fallback hardcoded**. Se DB irraggiungibile o tabella vuota per
il provider, solleva `ProviderCatalogUnavailable` — il chiamante decide
se mostrare lista vuota all'admin o ritornare errore.
"""
from __future__ import annotations

import logging
import os
import time

from .base import ProviderCatalogEntry

logger = logging.getLogger(__name__)


class ProviderCatalogUnavailable(Exception):
    """Sollevata quando non e' possibile caricare il catalogo modelli di un
    provider (DB irraggiungibile o nessun modello configurato in
    `ai_price_catalog` per quel provider). Niente fallback hardcoded."""
    pass


# Cache 60s per provider
_CACHE: dict[str, list[ProviderCatalogEntry]] = {}
_CACHE_TS: dict[str, float] = {}
_TTL_S = 60.0


def _load_from_db(provider: str) -> list[ProviderCatalogEntry]:
    """Carica i modelli enabled per `provider` da `ai_price_catalog`.
    Solleva eccezione se DB irraggiungibile."""
    import psycopg2  # type: ignore[import]
    db_url = os.environ.get(
        "DATABASE_URL",
        "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable",
    )
    with psycopg2.connect(db_url) as conn:
        with conn.cursor() as cur:
            cur.execute(
                "SELECT model, capabilities FROM ai_price_catalog "
                "WHERE provider = %s AND is_enabled = TRUE "
                "ORDER BY is_featured DESC, input_cost_per_million_tokens ASC",
                (provider,),
            )
            rows = cur.fetchall()
    out: list[ProviderCatalogEntry] = []
    for model, capabilities in rows:
        # capabilities e' jsonb (lista) o NULL
        caps: list[str]
        if isinstance(capabilities, list):
            caps = [str(c) for c in capabilities]
        elif isinstance(capabilities, str):
            # Stringa JSON
            import json
            try:
                parsed = json.loads(capabilities)
                caps = [str(c) for c in parsed] if isinstance(parsed, list) else ["chat"]
            except Exception:
                caps = ["chat"]
        else:
            caps = ["chat"]
        out.append(ProviderCatalogEntry(model, caps))
    return out


def load_provider_catalog(provider: str) -> list[ProviderCatalogEntry]:
    """Restituisce la lista dei modelli per `provider`, letti da DB con cache 60s.
    Solleva `ProviderCatalogUnavailable` se DB irraggiungibile o nessun
    modello configurato per quel provider."""
    now = time.time()
    try:
        from brain.utils.settings_db import get_int_setting
        _ttl = float(get_int_setting("providers.catalog_cache_ttl_seconds", 60))
    except Exception:
        _ttl = _TTL_S
    if provider in _CACHE and (now - _CACHE_TS.get(provider, 0.0)) < _ttl:
        return _CACHE[provider]
    try:
        rows = _load_from_db(provider)
    except Exception as e:
        raise ProviderCatalogUnavailable(
            f"DB irraggiungibile per provider '{provider}': {e}. "
            "Verifica Postgres e tabella ai_price_catalog."
        )
    if not rows:
        raise ProviderCatalogUnavailable(
            f"Nessun modello configurato in ai_price_catalog per provider '{provider}'. "
            "Esegui INSERT con i modelli supportati."
        )
    _CACHE[provider] = rows
    _CACHE_TS[provider] = now
    return rows
