"""Provider registry managing all LLM providers."""
from __future__ import annotations

import asyncio
import json
import logging
import os
import time
from typing import Any

from .base import BaseProvider, ProviderCatalogEntry, ProviderResult
from .openai_provider import OpenAIProvider
from .anthropic_provider import AnthropicProvider
from .google_provider import GoogleProvider
from .deepseek_provider import DeepSeekProvider
from .mistral_provider import MistralProvider
from .ollama_provider import OllamaProvider
from .vllm_provider import VllmProvider
from brain.utils.db_pool import get_db_url

logger = logging.getLogger(__name__)

# ── Provider billing-error cooldown cache ─────────────────────────────────────
# Quando un provider ritorna billing_error (credit_balance_too_low, quota
# esaurita), il brain lo mette in cooldown locale per evitare di sprecare
# chiamate API che falliranno comunque. Il prossimo turno parte direttamente
# dal provider successivo nella chain.
# TTL conservativo: 10 min. La cache e' azzerata al restart del brain.
# Una soluzione piu' completa (con notifica UI via mcp-core) e' tracciata
# nel task #30 — qui implementiamo solo il cooldown lato brain per evitare
# il loop di chiamate Anthropic 400 ogni turno.
_PROVIDER_COOLDOWN_TTL_FALLBACK_S = 600  # solo fallback se DB down (regola G)
_provider_cooldown_until: dict[str, float] = {}
# Cache del set di provider in cooldown letto dal DB (persistente in
# nexus_provider_health, mig 0255). TTL breve per non interrogare a ogni turno.
_db_cooldown_set_cached: set[str] = set()
_db_cooldown_cache_ts: float = 0.0
_DB_COOLDOWN_CACHE_TTL_S = 30.0


def _cooldown_db_url() -> str:
    return get_db_url()


def _billing_cooldown_ttl_s() -> int:
    """TTL del cooldown billing (secondi), DB-driven (regola G). Default 600 se
    il setting manca o il DB e' irraggiungibile."""
    try:
        from brain.utils.settings_db import get_int_setting
        return get_int_setting("providers.billing_cooldown_seconds", _PROVIDER_COOLDOWN_TTL_FALLBACK_S)
    except Exception:
        return _PROVIDER_COOLDOWN_TTL_FALLBACK_S


def _db_cooldown_providers() -> set[str]:
    """Provider in billing-cooldown secondo il DB (persistente, cache 30s)."""
    global _db_cooldown_set_cached, _db_cooldown_cache_ts
    now = time.monotonic()
    if now - _db_cooldown_cache_ts < _DB_COOLDOWN_CACHE_TTL_S:
        return _db_cooldown_set_cached
    try:
        import psycopg2  # type: ignore[import]
        with psycopg2.connect(_cooldown_db_url()) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT provider FROM nexus_provider_health "
                    "WHERE billing_cooldown_until IS NOT NULL "
                    "  AND billing_cooldown_until > NOW()"
                )
                rows = cur.fetchall()
        _db_cooldown_set_cached = {str(r[0]).lower() for r in rows}
        _db_cooldown_cache_ts = now
    except Exception as e:
        logger.debug("cooldown DB read fallito: %s", e)
    return _db_cooldown_set_cached


def _gate_cooldown_providers() -> set[str] | None:
    """Provider in cooldown secondo il GATE Rust — fonte di verita' unica a
    runtime (ADR 0020).

    Il gate accumula sia i cooldown che osserva direttamente sia quelli che il
    brain stesso gli riporta via `provider-error` (cooldown_bridge). Consultarlo
    qui fa convergere la cascade-fallback del brain sulla stessa vista del gate,
    eliminando la divergenza storica tra i due store (era la causa per cui il
    brain ritentava anthropic/openai gia' noti morti). La logica locale
    (in-memory + DB `nexus_provider_health`) resta come writer del bridge e come
    degrado offline se il gate e' irraggiungibile, NON come fonte primaria.

    Ritorna `None` se il gate non risponde (il caller usa la vista locale).
    """
    try:
        from brain.router.service import _routing_client_singleton
        return _routing_client_singleton().cooldown_providers()
    except Exception as e:
        logger.debug("gate cooldown read fallito: %s", e)
        return None


def _is_in_billing_cooldown(provider: str) -> bool:
    """True se il provider e' in cooldown billing/quota attivo.

    Ordine delle fonti (ADR 0020): GATE Rust (autoritativo) -> in-memory locale
    -> DB `nexus_provider_health` (degrado offline). Un provider e' considerato
    in cooldown se QUALSIASI fonte autoritativa lo segnala: cosi' la cascade
    fallback non ritenta mai un provider che il gate considera morto.
    """
    key = provider.lower()
    # Fonte primaria: gate Rust (include i cooldown riportati dal brain stesso).
    gate = _gate_cooldown_providers()
    if gate is not None and key in gate:
        return True
    # Vista locale in-memory (writer del bridge; valida anche se il gate e' giu').
    until = _provider_cooldown_until.get(key)
    if until is not None:
        if time.monotonic() < until:
            return True
        _provider_cooldown_until.pop(key, None)
    # Degrado: DB persistente, usato soprattutto quando il gate non risponde.
    return key in _db_cooldown_providers()


def _mark_billing_cooldown(provider: str, reason: str = "billing_error") -> None:
    """Registra un provider in cooldown (in-memory + DB persistente, mig 0255)."""
    global _db_cooldown_cache_ts
    key = provider.lower()
    ttl = _billing_cooldown_ttl_s()
    _provider_cooldown_until[key] = time.monotonic() + ttl
    try:
        import psycopg2  # type: ignore[import]
        with psycopg2.connect(_cooldown_db_url()) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "INSERT INTO nexus_provider_health "
                    "(provider, billing_cooldown_until, last_error, updated_at) "
                    "VALUES (%s, NOW() + (%s || ' seconds')::interval, %s, NOW()) "
                    "ON CONFLICT (provider) DO UPDATE SET "
                    "  billing_cooldown_until = EXCLUDED.billing_cooldown_until, "
                    "  last_error = EXCLUDED.last_error, updated_at = NOW()",
                    (key, str(ttl), reason),
                )
            conn.commit()
        _db_cooldown_cache_ts = 0.0  # invalida cache
    except Exception as e:
        logger.debug("cooldown DB write fallito: %s", e)
    logger.warning(
        "Provider %s in billing-cooldown (%s) per %ds (skip nelle prossime richieste)",
        provider, reason, ttl,
    )


def _clear_billing_cooldown(provider: str) -> None:
    """Ripristino automatico: azzera il cooldown del provider al primo successo."""
    global _db_cooldown_cache_ts
    key = provider.lower()
    had_mem = _provider_cooldown_until.pop(key, None) is not None
    if not had_mem and key not in _db_cooldown_set_cached:
        return
    try:
        import psycopg2  # type: ignore[import]
        with psycopg2.connect(_cooldown_db_url()) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "UPDATE nexus_provider_health "
                    "SET billing_cooldown_until = NULL, updated_at = NOW() "
                    "WHERE provider = %s AND billing_cooldown_until IS NOT NULL",
                    (key,),
                )
            conn.commit()
        _db_cooldown_cache_ts = 0.0
        logger.info("Provider %s ripristinato (cooldown azzerato dopo un 200)", provider)
    except Exception as e:
        logger.debug("cooldown DB clear fallito: %s", e)


def get_billing_cooldown_snapshot() -> dict[str, int]:
    """Restituisce {provider: secondi_rimanenti} per i provider in cooldown.

    Usato dall'endpoint REST /providers/billing-cooldown per esporre lo stato
    a mcp-core (che a sua volta aggiorna i LED nella UI).
    """
    now = time.monotonic()
    snapshot: dict[str, int] = {}
    for key, until in list(_provider_cooldown_until.items()):
        remaining = int(until - now)
        if remaining > 0:
            snapshot[key] = remaining
        else:
            _provider_cooldown_until.pop(key, None)
    return snapshot


# ── M7 Q-value: salute provider per intent ────────────────────────────────────
# Registra l'esito di ogni turno per (provider, model, intent) su
# nexus_provider_intent_health e, su soglia di fallimenti, mette il provider in
# cooldown su quell'intent. Tutto gated da routing.intent_health_enabled
# (default OFF): con il flag spento queste funzioni sono no-op (zero overhead,
# zero rischio sul routing). Soglie DB-driven (regola G).

_RETRIABLE_OUTCOME_STOPS = {
    "billing_error", "rate_limit", "overloaded", "provider_error", "error", "timeout",
}


def _intent_health_enabled() -> bool:
    try:
        from brain.utils.settings_db import get_bool_setting
        return get_bool_setting("routing.intent_health_enabled", False)
    except Exception:
        return False


def _classify_outcome(res: "ProviderResult") -> str:
    """Classifica l'esito di un turno: 'failure' | 'soft_failure' | 'success'."""
    meta = res.metadata or {}
    stop = meta.get("stop_reason")
    if stop in _RETRIABLE_OUTCOME_STOPS or (res.content or "").startswith("[Error"):
        return "failure"
    blocks = meta.get("tool_use_blocks") or []
    if stop in ("end_turn", "stop", "", None) and not blocks and len(res.content or "") < 40:
        return "soft_failure"
    return "success"


def _record_intent_health(provider: str, model: str, intent: str, outcome: str) -> None:
    """UPSERT del contatore esito per (provider, model, intent) e cooldown su
    soglia. No-op se il flag M7 e' OFF. Best-effort: mai solleva."""
    if not provider or not model or not _intent_health_enabled():
        return
    intent = (intent or "chat").strip() or "chat"
    col = {
        "success": "success_count",
        "failure": "failure_count",
        "soft_failure": "soft_failure_count",
    }.get(outcome)
    if col is None:
        return
    try:
        from brain.utils.settings_db import get_int_setting
        min_attempts = get_int_setting("routing.intent_health_min_attempts", 8)
        fail_pct = get_int_setting("routing.intent_health_failure_threshold_pct", 60)
        cooldown_secs = get_int_setting("routing.intent_health_cooldown_secs", 600)
    except Exception:
        min_attempts, fail_pct, cooldown_secs = 8, 60, 600

    last_ts_col = "last_success_at" if outcome == "success" else "last_failure_at"
    import psycopg2  # type: ignore[import]
    try:
        db_url = get_db_url()
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    f"INSERT INTO nexus_provider_intent_health "
                    f"(provider, model, intent_subkind, {col}, last_seen_at, {last_ts_col}, "
                    f" created_at, updated_at) "
                    f"VALUES (%s, %s, %s, 1, NOW(), NOW(), NOW(), NOW()) "
                    f"ON CONFLICT (provider, model, intent_subkind) DO UPDATE SET "
                    f"  {col} = nexus_provider_intent_health.{col} + 1, "
                    f"  last_seen_at = NOW(), {last_ts_col} = NOW(), updated_at = NOW() "
                    f"RETURNING success_count, failure_count, soft_failure_count",
                    (provider, model, intent),
                )
                row = cur.fetchone()
                if row and outcome != "success":
                    succ, fail, soft = int(row[0]), int(row[1]), int(row[2])
                    total = succ + fail + soft
                    bad = fail + soft
                    if total >= min_attempts and bad * 100 >= fail_pct * total:
                        cur.execute(
                            "UPDATE nexus_provider_intent_health "
                            "SET cooldown_until = NOW() + (%s || ' seconds')::interval, "
                            "    cooldown_reason = 'intent_failure_rate' "
                            "WHERE provider = %s AND model = %s AND intent_subkind = %s",
                            (str(cooldown_secs), provider, model, intent),
                        )
                        logger.warning(
                            "M7: %s/%s in cooldown su intent '%s' (%d/%d fallimenti)",
                            provider, model, intent, bad, total,
                        )
            conn.commit()
    except Exception as e:
        logger.debug("intent_health record fallito %s/%s/%s: %s", provider, model, intent, e)


def _intent_in_cooldown(provider: str, intent: str) -> bool:
    """True se almeno un modello del provider e' in cooldown M7 attivo su questo
    intent. No-op (False) se il flag M7 e' OFF. Best-effort."""
    if not provider or not _intent_health_enabled():
        return False
    intent = (intent or "chat").strip() or "chat"
    import psycopg2  # type: ignore[import]
    try:
        db_url = get_db_url()
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT 1 FROM nexus_provider_intent_health "
                    "WHERE provider = %s AND intent_subkind = %s "
                    "  AND cooldown_until IS NOT NULL AND cooldown_until > NOW() LIMIT 1",
                    (provider, intent),
                )
                return cur.fetchone() is not None
    except Exception:
        return False


# ── Billing enabled ───────────────────────────────────────────────────────────
# Valore canonico: settings.brain_billing_enabled nel DB (admin panel).
# Override emergenza: NEXUS_BRAIN_BILLING=on (priorita' massima).
def _brain_billing_enabled() -> bool:
    """Restituisce True se il ledger billing e' attivo."""
    from brain.utils.settings_db import get_bool_setting as _gbs
    env_val = os.environ.get("NEXUS_BRAIN_BILLING", "").strip().lower()
    if env_val in ("on", "1", "true"):
        return True
    if env_val in ("off", "0", "false"):
        return False
    # Nessun env var: leggi dal DB
    return _gbs("brain_billing_enabled", False)

_BILLING_CTX_CACHE: tuple[str, str] | None = None
_BILLING_CTX_TS: float = 0.0


def _billing_context() -> tuple[str, str]:
    """Ritorna (user_id, project_id) come UUID string.

    Il gRPC attuale non trasporta user/project: per non perdere telemetria,
    usiamo un contesto "di sistema" (ultimo utente + ultimo progetto). Cache 30s.
    """
    global _BILLING_CTX_CACHE, _BILLING_CTX_TS
    now = time.time()
    if _BILLING_CTX_CACHE and (now - _BILLING_CTX_TS) < 30.0:
        return _BILLING_CTX_CACHE
    import psycopg2  # type: ignore[import]
    db_url = get_db_url()
    with psycopg2.connect(db_url) as conn:
        with conn.cursor() as cur:
            cur.execute("SELECT id::text FROM users ORDER BY created_at DESC LIMIT 1")
            user_row = cur.fetchone()
            cur.execute("SELECT id::text FROM projects ORDER BY created_at DESC LIMIT 1")
            project_row = cur.fetchone()
    user_id = (user_row[0] if user_row else "") or ""
    project_id = (project_row[0] if project_row else "") or ""
    _BILLING_CTX_CACHE = (user_id, project_id)
    _BILLING_CTX_TS = now
    return _BILLING_CTX_CACHE


def _lookup_price_any_currency(provider: str, model: str) -> tuple[float, float, float, float, str]:
    """Ritorna (in_cost_per_mtok, out_cost_per_mtok, cache_read_per_mtok, cache_creation_per_mtok, currency).

    Legge anche i prezzi cache (0130_price_cache_columns). Fallback 0 se non trovato.
    """
    import psycopg2  # type: ignore[import]
    db_url = get_db_url()
    try:
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT input_cost_per_million_tokens, output_cost_per_million_tokens, "
                    "COALESCE(cache_read_cost_per_million_tokens, 0), "
                    "COALESCE(cache_creation_cost_per_million_tokens, 0), "
                    "currency "
                    "FROM ai_price_catalog "
                    "WHERE provider = %s AND model = %s AND is_enabled = TRUE "
                    "ORDER BY effective_from DESC LIMIT 1",
                    (provider, model),
                )
                row = cur.fetchone()
        if row:
            return (
                float(row[0]),
                float(row[1]),
                float(row[2]),
                float(row[3]),
                str(row[4] or "EUR").strip().upper(),
            )
    except Exception as e:
        logger.warning("billing price lookup fallito %s/%s: %s", provider, model, e)
    return (0.0, 0.0, 0.0, 0.0, "EUR")


# UUID sistema usato come placeholder per chiamate senza contesto user/project nel gRPC.
# Permette di registrare il consumo nel ledger anche quando il context manca,
# invece di perdere silenziosamente la telemetria.
_SYSTEM_UUID = "00000000-0000-0000-0000-000000000000"


def _record_usage(provider: str, model: str, usage: dict[str, Any] | None, details: dict[str, Any]) -> None:
    """Scrive su ai_usage_ledger (best-effort).

    Registra i token e il costo effettivo incluso il caching Anthropic
    (cache_read_input_tokens = 0.1x, cache_creation_input_tokens = 1.25x).
    Se user_id/project_id non disponibili, usa UUID di sistema per non perdere telemetria.
    """
    if not _brain_billing_enabled():
        return
    if not usage:
        return

    user_id, project_id = _billing_context()
    no_context = not user_id or not project_id
    if no_context:
        # Invece di uscire silenziosamente, usa UUID sistema e flagga nei details
        user_id = _SYSTEM_UUID
        project_id = _SYSTEM_UUID
        logger.warning(
            "billing context mancante per %s/%s: uso UUID sistema per non perdere telemetria",
            provider, model,
        )

    prompt_tokens = int(usage.get("input_tokens") or usage.get("prompt_tokens") or 0)
    completion_tokens = int(usage.get("output_tokens") or usage.get("completion_tokens") or 0)
    total_tokens = int(usage.get("total_tokens") or (prompt_tokens + completion_tokens))

    # Token cache Anthropic (o altri provider con struttura analoga)
    cache_read_tokens = int(usage.get("cache_read_input_tokens", 0) or usage.get("cache_read_tokens", 0) or 0)
    cache_creation_tokens = int(usage.get("cache_creation_input_tokens", 0) or usage.get("cache_created_tokens", 0) or usage.get("cache_creation_tokens", 0) or 0)

    in_cost_m, out_cost_m, cache_read_m, cache_creation_m, currency = _lookup_price_any_currency(provider, model)
    input_cost = (prompt_tokens / 1_000_000.0) * in_cost_m
    output_cost = (completion_tokens / 1_000_000.0) * out_cost_m
    cache_read_cost = (cache_read_tokens / 1_000_000.0) * cache_read_m
    cache_creation_cost = (cache_creation_tokens / 1_000_000.0) * cache_creation_m
    total_cost = input_cost + output_cost + cache_read_cost + cache_creation_cost

    # run_id come colonna dedicata (non solo dentro details) per poter attribuire
    # il costo al run reale. La colonna e' UUID: un valore vuoto o non valido
    # diventa NULL invece di far fallire l'INSERT.
    import uuid as _uuid
    _run_id_raw = (details or {}).get("run_id") or ""
    try:
        run_id_col = str(_uuid.UUID(str(_run_id_raw))) if _run_id_raw else None
    except (ValueError, AttributeError, TypeError):
        run_id_col = None

    # Arricchisce i details con la rottura del costo per il debug
    enriched_details: dict[str, Any] = {
        **details,
        "no_billing_context": no_context,
    }
    if cache_read_tokens or cache_creation_tokens:
        enriched_details["cache_tokens"] = {
            "read": cache_read_tokens,
            "creation": cache_creation_tokens,
            "read_cost": round(cache_read_cost, 8),
            "creation_cost": round(cache_creation_cost, 8),
        }

    import psycopg2  # type: ignore[import]
    try:
        db_url = get_db_url()
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "INSERT INTO ai_usage_ledger "
                    "(user_id, project_id, run_id, provider, model, "
                    "prompt_tokens, completion_tokens, total_tokens, "
                    "cache_read_tokens, cache_creation_tokens, "
                    "input_cost, output_cost, total_cost, "
                    "cache_read_cost, cache_creation_cost, "
                    "currency, status, details) "
                    # run_id via subquery: se il run non e' (ancora) in agent_runs
                    # la subquery ritorna NULL invece di violare la FK
                    # ai_usage_ledger_run_id_fkey (il ledger del turno puo' essere
                    # scritto prima che la riga agent_runs sia persistita).
                    "VALUES (%s::uuid, %s::uuid, "
                    "(SELECT id FROM agent_runs WHERE id = %s::uuid), "
                    "%s, %s, %s, %s, %s, %s, %s, "
                    "%s, %s, %s, %s, %s, %s, 'finalized', %s::jsonb)",
                    (
                        user_id,
                        project_id,
                        run_id_col,
                        provider,
                        model,
                        prompt_tokens,
                        completion_tokens,
                        total_tokens,
                        cache_read_tokens,
                        cache_creation_tokens,
                        input_cost,
                        output_cost,
                        total_cost,
                        cache_read_cost,
                        cache_creation_cost,
                        currency,
                        json.dumps(enriched_details),
                    ),
                )
            conn.commit()
        logger.debug(
            "billing ledger: %s/%s prompt=%d compl=%d cache_read=%d cache_cre=%d cost=%.6f %s",
            provider, model,
            prompt_tokens, completion_tokens,
            cache_read_tokens, cache_creation_tokens,
            total_cost, currency,
        )
    except Exception as e:
        logger.warning("billing ledger insert fallito %s/%s: %s", provider, model, e)


def _enforce_quota_estimate(provider: str, model: str, estimated_prompt_tokens: int, estimated_completion_tokens: int) -> tuple[bool, str]:
    """Guardrail hard (best-effort): blocca PRIMA della chiamata se supereresti la quota.

    Nota: non abbiamo user/project nel gRPC → usiamo _billing_context() (contabilità di sistema).
    """
    if not _brain_billing_enabled():
        return (True, "")
    user_id, project_id = _billing_context()
    if not user_id or not project_id:
        return (True, "")
    est_total_tokens = max(0, int(estimated_prompt_tokens) + int(estimated_completion_tokens))
    in_cost_m, out_cost_m, _cr_m, _cc_m, currency = _lookup_price_any_currency(provider, model)
    est_cost = ((max(0, int(estimated_prompt_tokens)) / 1_000_000.0) * in_cost_m) + (
        (max(0, int(estimated_completion_tokens)) / 1_000_000.0) * out_cost_m
    )

    import psycopg2  # type: ignore[import]
    db_url = get_db_url()
    try:
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT scope_type, token_limit, cost_limit, valid_from, valid_to "
                    "FROM ai_quota_policies "
                    "WHERE is_enabled=TRUE AND valid_from <= NOW() AND valid_to > NOW() AND ("
                    " (scope_type='user' AND user_id=%s::uuid) OR "
                    " (scope_type='project' AND project_id=%s::uuid) OR "
                    " (scope_type='user_project' AND user_id=%s::uuid AND project_id=%s::uuid)"
                    ")",
                    (user_id, project_id, user_id, project_id),
                )
                quotas = cur.fetchall() or []
                for (scope_type, token_limit, cost_limit, valid_from, valid_to) in quotas:
                    cur.execute(
                        "SELECT COALESCE(SUM(total_tokens),0)::bigint, COALESCE(SUM(total_cost),0)::float8 "
                        "FROM ai_usage_ledger "
                        "WHERE status IN ('reserved','finalized') "
                        "AND created_at >= %s AND created_at < %s AND ("
                        " (%s='user' AND user_id=%s::uuid) OR "
                        " (%s='project' AND project_id=%s::uuid) OR "
                        " (%s='user_project' AND user_id=%s::uuid AND project_id=%s::uuid)"
                        ")",
                        (valid_from, valid_to, scope_type, user_id, scope_type, project_id, scope_type, user_id, project_id),
                    )
                    used_tokens, used_cost = cur.fetchone() or (0, 0.0)
                    if token_limit is not None and (int(used_tokens) + est_total_tokens) > int(token_limit):
                        return (False, f"Quota token superata ({used_tokens}+{est_total_tokens}/{token_limit})")
                    if cost_limit is not None and (float(used_cost) + est_cost) > float(cost_limit):
                        return (False, f"Quota costo superata ({used_cost:.4f}+{est_cost:.4f}/{cost_limit} {currency})")
    except Exception as e:
        logger.warning("quota check fallito (skip): %s", e)
    return (True, "")


def _close_loop_safely(loop: "asyncio.AbstractEventLoop", provider_obj: object | None = None) -> None:
    """Chiude un event loop dedicato senza lasciare 'Event loop is closed'.

    I provider che usano client async basati su httpx/HTTP-2 (es. google-genai)
    schedulano task di teardown delle connessioni alla chiusura. Se il loop
    viene chiuso prima che questi completino, i loro callback girano su un loop
    gia' chiuso e asyncio logga 'Task exception was never retrieved ...
    RuntimeError: Event loop is closed'. Sequenza corretta, sul loop ANCORA
    aperto:
      1. chiudi i client async del provider legati a questo loop;
      2. chiudi gli async generator;
      3. cancella e drena i task residui;
      4. solo allora chiudi il loop.
    """
    if provider_obj is not None and hasattr(provider_obj, "aclose_current_loop_clients"):
        try:
            loop.run_until_complete(provider_obj.aclose_current_loop_clients())  # type: ignore[attr-defined]
        except Exception:
            pass
    try:
        loop.run_until_complete(loop.shutdown_asyncgens())
    except Exception:
        pass
    try:
        pending = asyncio.all_tasks(loop)
        if pending:
            for task in pending:
                task.cancel()
            loop.run_until_complete(asyncio.gather(*pending, return_exceptions=True))
    except Exception:
        pass
    loop.close()


class ProviderRegistry:
    def __init__(self) -> None:
        self._providers: dict[str, BaseProvider] = {
            "openai": OpenAIProvider(),
            "anthropic": AnthropicProvider(),
            "google": GoogleProvider(),
            "deepseek": DeepSeekProvider(),
            "mistral": MistralProvider(),
            "ollama": OllamaProvider(),   # Provider locale on-premise — zero cloud
            "vllm": VllmProvider(),       # Provider vLLM via OpenAI-compat API (profile onprem)
        }
        # Tutti i provider abilitati di default; _load_keys_from_db() può sovrascrivere
        self._disabled: set[str] = set()

    def set_enabled(self, name: str, enabled: bool) -> None:
        """Abilita o disabilita un provider. Thread-safe (GIL)."""
        if enabled:
            self._disabled.discard(name)
        else:
            self._disabled.add(name)
        logger.info("Provider %s: %s", name, "abilitato" if enabled else "disabilitato")

    def is_enabled(self, name: str) -> bool:
        return name not in self._disabled

    def get_provider(self, name: str) -> BaseProvider | None:
        return self._providers.get(name)

    def list_models(self, provider: str) -> list[ProviderCatalogEntry]:
        p = self._providers.get(provider)
        return p.list_models() if p else []

    def sync_models(self, provider: str) -> dict[str, object]:
        models = self.list_models(provider)
        return {
            "provider": provider,
            "status": "synced",
            "models": [entry.id for entry in models],
        }

    def test_connection(self, provider: str) -> dict[str, object]:
        if not self.is_enabled(provider):
            return {"provider": provider, "status": "disabled"}
        p = self._providers.get(provider)
        if p is None:
            return {"provider": provider, "status": "unknown", "skipReasons": ["provider_not_configured"]}
        try:
            import concurrent.futures
            def _run():
                loop = asyncio.new_event_loop()
                asyncio.set_event_loop(loop)
                try:
                    return loop.run_until_complete(p.test_connection())
                finally:
                    _close_loop_safely(loop, p)
            with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                return pool.submit(_run).result(timeout=15)
        except Exception as e:
            return {"provider": provider, "status": "error", "reason": str(e)}

    def generate_completion(self, provider: str, model: str, prompt: str) -> ProviderResult:
        if not self.is_enabled(provider):
            return ProviderResult(
                provider=provider, model=model,
                content=f"[Provider '{provider}' è disabilitato]",
                metadata={"error": "provider_disabled"},
            )
        p = self._providers.get(provider)
        if p is None:
            return ProviderResult(
                provider=provider, model=model,
                content=f"[Provider '{provider}' not found]",
                metadata={"error": "unknown_provider"},
            )
        ok, reason = _enforce_quota_estimate(provider, model, estimated_prompt_tokens=max(1, len(prompt) // 4), estimated_completion_tokens=800)
        if not ok:
            return ProviderResult(
                provider=provider,
                model=model,
                content=f"[Quota superata: {reason}]",
                metadata={"error": "quota_exceeded", "stop_reason": "billing_error"},
            )
        try:
            import concurrent.futures
            def _run():
                loop = asyncio.new_event_loop()
                asyncio.set_event_loop(loop)
                try:
                    return loop.run_until_complete(p.generate(model, prompt))
                finally:
                    _close_loop_safely(loop, p)
            with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                result = pool.submit(_run).result(timeout=60)
                usage = (result.metadata or {}).get("usage")
                _record_usage(provider, model, usage if isinstance(usage, dict) else None, {"feature": "neural.GenerateCompletion"})
                return result
        except Exception as e:
            logger.error("Completion failed for %s/%s: %s", provider, model, e)
            return ProviderResult(
                provider=provider, model=model,
                content=f"[Error: {e}]",
                metadata={"error": str(e)},
            )

    def _provider_fallback_chain(self, exclude: str | None = None) -> list[str]:
        """Ordine dinamico dei provider di fallback letti da DB (settings.provider_hierarchy)
        o, se assente, dall'ordine alfabetico dei provider abilitati con `generate_agent_turn`.

        Niente hardcoded: la regola G del CLAUDE.md vieta `["anthropic","openai"]`
        come fallback magico — l'admin puo' configurare la priorita' via DB.
        """
        try:
            import psycopg2  # type: ignore[import]
            import os
            db_url = get_db_url()
            with psycopg2.connect(db_url) as conn:
                with conn.cursor() as cur:
                    cur.execute(
                        "SELECT value FROM settings WHERE key = 'provider_hierarchy' LIMIT 1"
                    )
                    row = cur.fetchone()
            if row and row[0]:
                names = [s.strip() for s in str(row[0]).split(",") if s.strip()]
                return [
                    n for n in names
                    if n != exclude and self.is_enabled(n)
                    and self._providers.get(n)
                    and hasattr(self._providers.get(n), "generate_agent_turn")
                ]
        except Exception as e:
            logger.warning("provider_hierarchy non leggibile da DB: %s", e)
        return [
            n for n in sorted(self._providers.keys())
            if n != exclude and self.is_enabled(n)
            and hasattr(self._providers.get(n), "generate_agent_turn")
        ]

    def _default_model_or_none(self, provider: str) -> str | None:
        """Lookup default model da DB (catalog_loader). None se non configurato."""
        try:
            from brain.grpc_server.neural_service import _default_model_for_provider
            return _default_model_for_provider(provider)
        except Exception as e:
            logger.warning("default_model lookup fallito per '%s': %s", provider, e)
            return None

    @staticmethod
    def _model_belongs_to_provider(provider: str, model: str) -> bool:
        """Guard-rail anti-mismatch: verifica che model appartenga a provider.

        Fonte di verita della coppia provider/model: nexus_provider_default_model
        (regola G CLAUDE.md). Questi prefix sono solo detection difensiva per
        impedire una coppia impossibile (es. anthropic + gemini-2.5-pro) che
        fallirebbe con 404 invalid_model. Vedi ADR 0016.
        """
        p = (provider or "").strip().lower()
        m = (model or "").strip().lower()
        if p == "anthropic":
            return m.startswith("claude")
        if p == "google":
            return m.startswith("gemini")
        if p == "openai":
            if m.startswith(("gpt", "o1", "o3", "o4")):
                return True
            return len(m) >= 2 and m[0] == "o" and m[1].isdigit()
        if p == "deepseek":
            return m.startswith("deepseek")
        if p == "mistral":
            return m.startswith(("mistral", "codestral", "ministral", "pixtral"))
        # Provider non riconosciuto: non blocchiamo (la verita resta il DB).
        return True

    def generate_agent_turn_sync(
        self,
        provider: str,
        model: str,
        messages: list[dict],
        tools: list[dict],
        max_tokens: int = 4096,
        system_text: str = "",
        usage_run_id: str = "",
        usage_iteration: int = 0,
        usage_intent: str = "",
        force_tool_choice: bool | None = None,
        soft_failure_fallback: bool = True,
    ) -> ProviderResult:
        """Versione sincrona di generate_agent_turn, sicura da chiamare da thread gRPC.

        ``force_tool_choice`` (ADR 0018 leva 2): override del tool_choice del
        provider. True forza una tool call (turni d'azione); False disattiva la
        forzatura (retry-senza-forcing dopo un MALFORMED_FUNCTION_CALL); None
        lascia la decisione storica per-provider (first_turn_force da capability).
        Passato solo ai provider che espongono il parametro (gli adapter
        tool-mute lo ignorano).

        ``soft_failure_fallback`` (ADR 0025): abilita la cascata su soft-failure
        M4 (chiusura naturale senza tool, contenuto sotto soglia). E' una
        euristica pensata per l'EXECUTOR (un modello che "molla" senza usare i
        tool a inizio task). Va disattivata (`False`) per chiamate con
        tool_choice forzato e fallback dedicato — il planner_node forza
        `nexus_todo_write` e gestisce in proprio il retry tool-robust via
        `purpose_model('planner_fallback')`: senza questo flag la cascata M4
        generica escalerebbe ai default model dei provider (es. gemini-2.5-pro)
        producendo l'apparenza di "completamento vuoto". Il fallback su errori
        retriable reali (billing/quota/timeout) resta sempre attivo.

        ``usage_run_id``/``usage_iteration``: contesto opzionale registrato nel
        ledger (colonna run_id + details). Quando l'executor LangGraph chiama
        questa funzione li valorizza, cosi' l'usage viene contato UNA sola volta
        qui (sia il tentativo primario sia l'eventuale fallback) ed e'
        attribuibile al run. Il path gRPC li lascia vuoti.

        Fallback dinamico: provider preferito disabilitato o non compatibile con tool_use
        → cerca nella `_provider_fallback_chain()` (DB-driven) il prossimo abilitato.
        Niente modelli hardcoded: i default model vengono da `nexus_provider_default_model`.
        """
        effective_provider = provider
        effective_model = model

        # NB: Google/Vertex supporta tool_use nativo (function calling) per i
        # modelli Gemini 1.5+ e 2.x via FunctionDeclaration, vedi
        # google_provider.py:generate_agent_turn. Il vecchio fallback forzato
        # da `provider == "google"` era una assunzione legacy ormai falsa che
        # produceva cascade rotti quando anthropic era in cooldown billing.
        # Il flusso ora prosegue al check `is_enabled` standard e, in caso di
        # errore reale dal provider, al cascade fallback alla fine della funzione.

        # Se il provider effettivo e' disabilitato, cerca nella chain il primo
        # provider con default model valido E coerente. Un provider senza default
        # model viene SKIPPATO, mai accoppiato al model originale. Vedi ADR 0016.
        if not self.is_enabled(effective_provider):
            fb_provider = None
            fb_model = None
            for cand in self._provider_fallback_chain(exclude=effective_provider):
                cand_model = self._default_model_or_none(cand)
                if cand_model is None:
                    logger.warning(
                        "Skip fallback %s: nessun default model in nexus_provider_default_model",
                        cand,
                    )
                    continue
                if not self._model_belongs_to_provider(cand, cand_model):
                    logger.error(
                        "Skip fallback %s: coppia incoerente con model %s",
                        cand, cand_model,
                    )
                    continue
                fb_provider, fb_model = cand, cand_model
                break
            if fb_provider:
                logger.warning(
                    "Provider %s disabilitato, fallback a %s/%s",
                    effective_provider, fb_provider, fb_model
                )
                effective_provider = fb_provider
                effective_model = fb_model
            else:
                return ProviderResult(
                    provider=effective_provider, model=effective_model,
                    content="[Nessun provider abilitato disponibile per agent turn]",
                    metadata={"error": "all_providers_disabled", "stop_reason": "error"},
                )

        p = self._providers.get(effective_provider)
        if p is None:
            return ProviderResult(
                provider=effective_provider, model=effective_model,
                content=f"[Provider '{effective_provider}' not found]",
                metadata={"error": "unknown_provider"},
            )
        if not hasattr(p, "generate_agent_turn"):
            return ProviderResult(
                provider=effective_provider, model=effective_model,
                content=f"[Provider '{effective_provider}' non supporta agent turn]",
                metadata={"error": "agent_turn_not_supported"},
            )
        def _run_agent_turn(prov_name: str, prov_model: str) -> ProviderResult:
            prov = self._providers.get(prov_name)
            if prov is None or not hasattr(prov, "generate_agent_turn"):
                return ProviderResult(
                    provider=prov_name, model=prov_model,
                    content=f"[Provider '{prov_name}' non disponibile per agent turn]",
                    metadata={"error": "provider_unavailable", "stop_reason": "error"},
                )
            try:
                ok, reason = _enforce_quota_estimate(
                    prov_name,
                    prov_model,
                    estimated_prompt_tokens=max(1, int(sum(len(str(m.get("content", ""))) for m in messages)) // 4),
                    estimated_completion_tokens=int(max_tokens or 0),
                )
                if not ok:
                    return ProviderResult(
                        provider=prov_name,
                        model=prov_model,
                        content=f"[Quota superata: {reason}]",
                        metadata={"error": "quota_exceeded", "stop_reason": "billing_error"},
                    )
                import concurrent.futures
                # ADR 0018 (b): passa force_tool_choice solo ai provider che
                # espongono il kwarg (anthropic/openai/google/mistral/deepseek).
                # I provider tool-mute o legacy (ollama/vllm/base) non lo hanno:
                # passarlo solleverebbe TypeError -> introspezione difensiva.
                _agent_kwargs: dict[str, Any] = {"system_text": system_text}
                if force_tool_choice is not None:
                    try:
                        import inspect as _inspect
                        _sig = _inspect.signature(prov.generate_agent_turn)
                        if "force_tool_choice" in _sig.parameters:
                            _agent_kwargs["force_tool_choice"] = force_tool_choice
                    except (TypeError, ValueError):
                        pass
                def _run():
                    loop = asyncio.new_event_loop()
                    asyncio.set_event_loop(loop)
                    try:
                        return loop.run_until_complete(
                            prov.generate_agent_turn(prov_model, messages, tools, max_tokens, **_agent_kwargs)
                        )
                    finally:
                        # Teardown sicuro: chiude i client async del provider e
                        # drena i task residui PRIMA di chiudere il loop, cosi'
                        # il teardown HTTP/2 di google-genai non finisce su un
                        # loop chiuso ("Event loop is closed").
                        _close_loop_safely(loop, prov)
                with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                    return pool.submit(_run).result(timeout=90)
            except Exception as exc:
                # M73: alcune eccezioni (es. TimeoutError, CancelledError) hanno
                # str(exc) vuoto: usiamo repr(exc) per garantire un messaggio
                # informativo e exc_info=True per il traceback completo.
                exc_msg = repr(exc) if not str(exc).strip() else str(exc)
                logger.error(
                    "Agent turn failed for %s/%s: %s",
                    prov_name, prov_model, exc_msg,
                    exc_info=True,
                )
                # Classifica timeout in modo distinto cosi' il cascade fallback
                # capisce il motivo del fallimento.
                import concurrent.futures as _cf
                if isinstance(exc, (_cf.TimeoutError, asyncio.TimeoutError)):
                    stop = "timeout"
                else:
                    stop = "error"
                return ProviderResult(
                    provider=prov_name, model=prov_model,
                    content=f"[Error: {exc_msg}]",
                    metadata={"error": exc_msg, "stop_reason": stop},
                )

        # Se il provider primario e' in billing-error cooldown locale, skip
        # direttamente al fallback senza sprecare una chiamata API. Il brain
        # ha gia' visto questo provider fallire con quota esaurita di recente.
        if _is_in_billing_cooldown(effective_provider):
            logger.warning(
                "Provider %s/%s in billing-cooldown locale, skip al fallback chain",
                effective_provider, effective_model,
            )
            result = ProviderResult(
                provider=effective_provider, model=effective_model,
                content=f"[Provider {effective_provider} in billing cooldown]",
                metadata={"error": "billing_cooldown_skip", "stop_reason": "billing_error"},
            )
        else:
            result = _run_agent_turn(effective_provider, effective_model)
            usage = (result.metadata or {}).get("usage")
            _record_usage(
                result.provider,
                result.model,
                usage if isinstance(usage, dict) else None,
                {
                    "feature": "neural.GenerateAgentTurn",
                    "run_id": usage_run_id,
                    "iteration": usage_iteration,
                },
            )
            # M7 — registra l'esito per (provider, model, intent). No-op se OFF.
            _record_intent_health(
                result.provider, result.model, usage_intent, _classify_outcome(result)
            )
            # Se billing_error, marca il provider in cooldown locale.
            # billing_error puo' essere su stop_reason (chain interna) o
            # error_class (format_error_result usa stop_reason="error" generico
            # e popola error_class con la classificazione fine).
            if (
                result.metadata.get("stop_reason") == "billing_error"
                or result.metadata.get("error_class") == "billing_error"
            ):
                _mark_billing_cooldown(effective_provider)
            elif result.metadata.get("stop_reason") not in (
                "rate_limit", "overloaded", "provider_error", "error", "timeout",
            ):
                # Chiamata andata a buon fine: ripristina il provider (auto-enable).
                _clear_billing_cooldown(effective_provider)

        # Fallback a cascata: se il provider fallisce per errori retriable (billing/quota/errore),
        # proviamo i provider successivi nella chain dinamica (DB-driven, niente hardcoded).
        _RETRIABLE_STOPS = {"billing_error", "rate_limit", "overloaded", "provider_error", "error", "timeout"}

        def _should_fallback(res: ProviderResult) -> bool:
            """Fallback se errore retriable OPPURE soft-failure (M4): chiusura
            naturale senza tool e con contenuto sotto soglia. Il soft-failure e
            best-effort: se la capability manca, viene ignorato (nessun crash)."""
            if res.metadata.get("stop_reason") in _RETRIABLE_STOPS or res.content.startswith("[Error:"):
                return True
            # ADR 0025: la cascata su soft-failure M4 e' un'euristica executor.
            # Per chiamate con tool_choice forzato + fallback dedicato (planner)
            # va disattivata: altrimenti escala ai default model dei provider
            # (gemini-2.5-pro) generando l'apparenza di "completamento vuoto".
            if not soft_failure_fallback:
                return False
            try:
                from .capability_loader import load_capability
                from .adapter_base import is_soft_failure
                from ._schema_utils import is_first_agent_turn
                _cap = load_capability(res.provider, res.model)
                if is_soft_failure(
                    res.metadata, res.content, _cap,
                    first_turn=is_first_agent_turn(messages),
                    intent=usage_intent,
                ):
                    logger.warning(
                        "Soft-failure %s/%s: chiusura naturale senza tool e contenuto "
                        "sotto soglia -> fallback (M4)", res.provider, res.model,
                    )
                    return True
            except Exception as _sf_err:
                logger.debug(
                    "soft-failure check saltato per %s/%s: %s",
                    res.provider, res.model, _sf_err,
                )
            return False

        if _should_fallback(result):
            for fb_prov in self._provider_fallback_chain(exclude=effective_provider):
                # Skip provider gia' in billing-cooldown locale.
                if _is_in_billing_cooldown(fb_prov):
                    logger.info(
                        "Fallback skip %s: in billing-cooldown locale", fb_prov,
                    )
                    continue
                # M7 — Skip provider in cooldown per questo intent (gated). No-op se OFF.
                if _intent_in_cooldown(fb_prov, usage_intent):
                    logger.info(
                        "Fallback skip %s: in cooldown M7 sull'intent '%s'",
                        fb_prov, usage_intent or "chat",
                    )
                    continue
                fb_model = self._default_model_or_none(fb_prov)
                if fb_model is None:
                    logger.warning(
                        "Skip fallback %s: nessun default model in nexus_provider_default_model",
                        fb_prov,
                    )
                    continue
                # Guard-rail anti-mismatch: la coppia (provider, model) deve essere
                # coerente, altrimenti la chiamata fallirebbe con 404. Vedi ADR 0016.
                if not self._model_belongs_to_provider(fb_prov, fb_model):
                    logger.error(
                        "Skip fallback %s: coppia incoerente con model %s",
                        fb_prov, fb_model,
                    )
                    continue
                logger.warning(
                    "Provider %s/%s fallito (%s), fallback a %s/%s",
                    effective_provider, effective_model,
                    result.metadata.get("stop_reason", "error"),
                    fb_prov, fb_model,
                )
                fb_result = _run_agent_turn(fb_prov, fb_model)
                usage = (fb_result.metadata or {}).get("usage")
                _record_usage(
                    fb_result.provider,
                    fb_result.model,
                    usage if isinstance(usage, dict) else None,
                    {
                        "feature": "neural.GenerateAgentTurn",
                        "fallback": True,
                        "run_id": usage_run_id,
                        "iteration": usage_iteration,
                    },
                )
                # M7 — registra l'esito del fallback per (provider, model, intent).
                _record_intent_health(
                    fb_result.provider, fb_result.model, usage_intent,
                    _classify_outcome(fb_result),
                )
                # Se anche il fallback e' billing_error, marca anche lui.
                if (
                    fb_result.metadata.get("stop_reason") == "billing_error"
                    or fb_result.metadata.get("error_class") == "billing_error"
                ):
                    _mark_billing_cooldown(fb_prov)
                if not _should_fallback(fb_result):
                    # Fallback riuscito: ripristina il provider che ha risposto.
                    _clear_billing_cooldown(fb_prov)
                    return fb_result
                # Continua al prossimo fallback
                result = fb_result

        return result

    async def generate_completion_async(self, provider: str, model: str, prompt: str, **kwargs: Any) -> ProviderResult:
        if not self.is_enabled(provider):
            return ProviderResult(
                provider=provider, model=model,
                content=f"[Provider '{provider}' è disabilitato]",
                metadata={"error": "provider_disabled"},
            )
        p = self._providers.get(provider)
        if p is None:
            return ProviderResult(
                provider=provider, model=model,
                content=f"[Provider '{provider}' not found]",
                metadata={"error": "unknown_provider"},
            )
        return await p.generate(model, prompt, **kwargs)

    async def test_connection_async(self, provider: str) -> dict[str, Any]:
        if not self.is_enabled(provider):
            return {"provider": provider, "status": "disabled"}
        p = self._providers.get(provider)
        if p is None:
            return {"provider": provider, "status": "unknown"}
        return await p.test_connection()
