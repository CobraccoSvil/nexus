"""Helper condiviso per caricare la lista modelli supportati per provider dal DB.

Sostituisce gli array hardcoded `list_models()` in tutti i provider cloud
(anthropic, openai, google, mistral, deepseek).

Pattern: ogni provider chiama `load_provider_catalog(provider_name)` in
`list_models()`. Cache 60s in-process (via ``brain.utils.ttl_cache.TtlCache``,
punto unico, regola L / ADR 0026) per evitare query a ogni call.

Sorgente DB: tabella `ai_price_catalog` (provider, model, capabilities,
is_enabled). Quando un modello viene aggiunto/rimosso/deprecato, basta
UPDATE/INSERT lì — niente patch + redeploy.

**Niente fallback hardcoded** (regola G): se ``DATABASE_URL`` non e' impostata
o il DB e' irraggiungibile o la tabella e' vuota per il provider, solleva
``ProviderCatalogUnavailable``. La connessione passa per
``brain.utils.db_pool.connect``: nessuna connection string copiata qui.
"""
from __future__ import annotations

import json
import logging

from brain.utils.db_pool import DbUrlUnavailable, connect
from brain.utils.ttl_cache import TtlCache

from .base import ProviderCatalogEntry

logger = logging.getLogger(__name__)


class ProviderCatalogUnavailable(Exception):
    """Sollevata quando non e' possibile caricare il catalogo modelli di un
    provider (DB irraggiungibile o nessun modello configurato in
    `ai_price_catalog` per quel provider). Niente fallback hardcoded."""
    pass


# Cache TTL 60s per provider (punto unico TtlCache).
_CACHE: TtlCache[str, list[ProviderCatalogEntry]] = TtlCache(ttl_seconds=60.0)


def _load_from_db(provider: str) -> list[ProviderCatalogEntry]:
    """Carica i modelli enabled per `provider` da `ai_price_catalog`.
    Solleva eccezione se DB irraggiungibile."""
    with connect() as conn, conn.cursor() as cur:
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
    cached = _CACHE.get(provider)
    if cached is not None:
        return cached
    try:
        rows = _load_from_db(provider)
    except DbUrlUnavailable as e:
        raise ProviderCatalogUnavailable(str(e)) from e
    except Exception as e:
        raise ProviderCatalogUnavailable(
            f"DB irraggiungibile per provider '{provider}': {e}. "
            "Verifica Postgres e tabella ai_price_catalog."
        ) from e
    if not rows:
        raise ProviderCatalogUnavailable(
            f"Nessun modello configurato in ai_price_catalog per provider '{provider}'. "
            "Esegui INSERT con i modelli supportati."
        )
    _CACHE.set(provider, rows)
    return rows
