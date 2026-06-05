"""Caricatore delle capability per-modello da nexus_provider_capabilities (mig 0240).

Fonte unica di verita per i parametri per-modello (max_tokens risposta,
tool_choice, dialetto schema, formato tool_call, cache prompt, timeout, soglie
soft-failure, compaction history, limiti tool_result). Regola G del CLAUDE.md:
nessun fallback hardcoded. Se manca la riga per un (provider, model) attivo,
solleva CapabilityUnavailable e il chiamante propaga l'errore (niente magia).

Risoluzione: prima (provider, model) esatto, poi (provider, '*') come default di
provider (per modelli dinamici tipo vllm/ollama, se configurato). Cache in-process
con TTL da settings.providers.capability_cache_ttl_seconds.
"""
from __future__ import annotations

import logging
import os
import time

from ._models import ProviderCapability
from brain.utils.db_pool import get_db_url

logger = logging.getLogger(__name__)


class CapabilityUnavailable(Exception):
    """Sollevata quando non esiste una riga capability per (provider, model)
    (ne esatta ne wildcard) o il DB e irraggiungibile. Niente fallback."""
    pass


# Cache per (provider, model) -> ProviderCapability
_CACHE: dict[tuple[str, str], ProviderCapability] = {}
_CACHE_TS: dict[tuple[str, str], float] = {}

# Colonne lette, nell'ordine del costruttore di ProviderCapability
_COLUMNS = (
    "provider, model, tool_use, vision, thinking, max_context_tokens, "
    "default_max_output_tokens, max_output_tokens_hard, tool_choice_style, "
    "tool_choice_first_turn_force, schema_strict, schema_dialect, tool_call_format, "
    "max_tools_in_request, supports_prompt_cache, prompt_cache_dialect, "
    "supports_parallel_tools, stop_reason_dialect, soft_failure_iter_threshold, "
    "soft_failure_content_threshold, history_keep_recent_messages, "
    "history_max_old_tool_result_chars, request_timeout_seconds, "
    "connect_timeout_seconds, tool_result_max_chars, tool_result_max_bytes, "
    "tool_result_max_lines, agentic_thinking_policy"
)

# TTL del TTL: evita una query a ogni lookup mantenendo la configurabilita.
_ttl_value: float = 60.0
_ttl_ts: float = 0.0
_TTL_REFRESH_S = 60.0


def _db_url() -> str:
    return get_db_url()


def _get_ttl() -> float:
    """TTL cache capability da settings.providers.capability_cache_ttl_seconds,
    riletto al massimo ogni 60s. Default 60 se DB/settings non disponibili."""
    global _ttl_value, _ttl_ts
    now = time.time()
    if (now - _ttl_ts) < _TTL_REFRESH_S:
        return _ttl_value
    try:
        from brain.utils.settings_db import get_int_setting
        _ttl_value = float(get_int_setting("providers.capability_cache_ttl_seconds", 60))
    except Exception:
        _ttl_value = 60.0
    _ttl_ts = now
    return _ttl_value


def _row_to_capability(row: tuple) -> ProviderCapability:
    return ProviderCapability(
        provider=row[0], model=row[1], tool_use=row[2], vision=row[3],
        thinking=row[4], max_context_tokens=row[5], default_max_output_tokens=row[6],
        max_output_tokens_hard=row[7], tool_choice_style=row[8],
        tool_choice_first_turn_force=row[9], schema_strict=row[10],
        schema_dialect=row[11], tool_call_format=row[12], max_tools_in_request=row[13],
        supports_prompt_cache=row[14], prompt_cache_dialect=row[15],
        supports_parallel_tools=row[16], stop_reason_dialect=row[17],
        soft_failure_iter_threshold=row[18], soft_failure_content_threshold=row[19],
        history_keep_recent_messages=row[20], history_max_old_tool_result_chars=row[21],
        request_timeout_seconds=row[22], connect_timeout_seconds=row[23],
        tool_result_max_chars=row[24], tool_result_max_bytes=row[25],
        tool_result_max_lines=row[26],
        agentic_thinking_policy=row[27] if len(row) > 27 else "none",
    )


def _load_from_db(provider: str, model: str) -> ProviderCapability | None:
    """Cerca (provider, model) esatto, poi (provider, '*'). None se nessuna riga."""
    import psycopg2  # type: ignore[import]
    with psycopg2.connect(_db_url()) as conn:
        with conn.cursor() as cur:
            cur.execute(
                f"SELECT {_COLUMNS} FROM v_model_capabilities "
                "WHERE provider = %s AND model = %s",
                (provider, model),
            )
            row = cur.fetchone()
            if row is None:
                cur.execute(
                    f"SELECT {_COLUMNS} FROM v_model_capabilities"
                    "WHERE provider = %s AND model = %s",
                    (provider, "*"),
                )
                row = cur.fetchone()
    return _row_to_capability(row) if row is not None else None


def load_capability(provider: str, model: str) -> ProviderCapability:
    """Capability per (provider, model) da DB, con cache TTL.
    Solleva CapabilityUnavailable se manca la riga o il DB e irraggiungibile."""
    key = (provider, model)
    now = time.time()
    if key in _CACHE and (now - _CACHE_TS.get(key, 0.0)) < _get_ttl():
        return _CACHE[key]
    try:
        cap = _load_from_db(provider, model)
    except Exception as e:
        raise CapabilityUnavailable(
            f"DB irraggiungibile per capability '{provider}/{model}': {e}. "
            "Verifica Postgres e tabella nexus_provider_capabilities (mig 0240)."
        )
    if cap is None:
        raise CapabilityUnavailable(
            f"Nessuna capability in nexus_provider_capabilities per '{provider}/{model}' "
            f"(ne riga esatta ne wildcard '{provider}/*'). "
            "Aggiungi la riga: il sistema non usa fallback hardcoded (regola G)."
        )
    _CACHE[key] = cap
    _CACHE_TS[key] = now
    return cap


def clear_cache() -> None:
    """Svuota la cache (utile nei test)."""
    _CACHE.clear()
    _CACHE_TS.clear()
