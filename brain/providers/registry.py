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

logger = logging.getLogger(__name__)

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
    db_url = os.environ.get(
        "DATABASE_URL",
        "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable",
    )
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


def _lookup_price_any_currency(provider: str, model: str) -> tuple[float, float, str]:
    """Ritorna (in_cost_per_mtok, out_cost_per_mtok, currency). Fallback 0 se non trovato."""
    import psycopg2  # type: ignore[import]
    db_url = os.environ.get(
        "DATABASE_URL",
        "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable",
    )
    try:
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT input_cost_per_million_tokens, output_cost_per_million_tokens, currency "
                    "FROM ai_price_catalog "
                    "WHERE provider = %s AND model = %s AND is_enabled = TRUE "
                    "ORDER BY effective_from DESC LIMIT 1",
                    (provider, model),
                )
                row = cur.fetchone()
        if row:
            return (float(row[0]), float(row[1]), str(row[2] or "EUR").strip().upper())
    except Exception as e:
        logger.warning("billing price lookup fallito %s/%s: %s", provider, model, e)
    return (0.0, 0.0, "EUR")


def _record_usage(provider: str, model: str, usage: dict[str, Any] | None, details: dict[str, Any]) -> None:
    """Scrive su ai_usage_ledger (best-effort)."""
    if os.environ.get("NEXUS_BRAIN_BILLING", "off").lower() != "on":
        return
    if not usage:
        return
    user_id, project_id = _billing_context()
    if not user_id or not project_id:
        return
    prompt_tokens = int(usage.get("input_tokens") or usage.get("prompt_tokens") or 0)
    completion_tokens = int(usage.get("output_tokens") or usage.get("completion_tokens") or 0)
    total_tokens = int(usage.get("total_tokens") or (prompt_tokens + completion_tokens))
    in_cost_m, out_cost_m, currency = _lookup_price_any_currency(provider, model)
    input_cost = (prompt_tokens / 1_000_000.0) * in_cost_m
    output_cost = (completion_tokens / 1_000_000.0) * out_cost_m
    total_cost = input_cost + output_cost

    import psycopg2  # type: ignore[import]
    try:
        db_url = os.environ.get(
            "DATABASE_URL",
            "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable",
        )
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "INSERT INTO ai_usage_ledger (user_id, project_id, provider, model, "
                    "prompt_tokens, completion_tokens, total_tokens, "
                    "input_cost, output_cost, total_cost, currency, status, details) "
                    "VALUES (%s::uuid, %s::uuid, %s, %s, %s, %s, %s, %s, %s, %s, %s, 'finalized', %s::jsonb)",
                    (
                        user_id,
                        project_id,
                        provider,
                        model,
                        prompt_tokens,
                        completion_tokens,
                        total_tokens,
                        input_cost,
                        output_cost,
                        total_cost,
                        currency,
                        json.dumps({**details, "missing_user_project_in_grpc": True}),
                    ),
                )
            conn.commit()
    except Exception as e:
        logger.warning("billing ledger insert fallito %s/%s: %s", provider, model, e)


def _enforce_quota_estimate(provider: str, model: str, estimated_prompt_tokens: int, estimated_completion_tokens: int) -> tuple[bool, str]:
    """Guardrail hard (best-effort): blocca PRIMA della chiamata se supereresti la quota.

    Nota: non abbiamo user/project nel gRPC → usiamo _billing_context() (contabilità di sistema).
    """
    if os.environ.get("NEXUS_BRAIN_BILLING", "off").lower() != "on":
        return (True, "")
    user_id, project_id = _billing_context()
    if not user_id or not project_id:
        return (True, "")
    est_total_tokens = max(0, int(estimated_prompt_tokens) + int(estimated_completion_tokens))
    in_cost_m, out_cost_m, currency = _lookup_price_any_currency(provider, model)
    est_cost = ((max(0, int(estimated_prompt_tokens)) / 1_000_000.0) * in_cost_m) + (
        (max(0, int(estimated_completion_tokens)) / 1_000_000.0) * out_cost_m
    )

    import psycopg2  # type: ignore[import]
    db_url = os.environ.get(
        "DATABASE_URL",
        "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable",
    )
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


class ProviderRegistry:
    def __init__(self) -> None:
        self._providers: dict[str, BaseProvider] = {
            "openai": OpenAIProvider(),
            "anthropic": AnthropicProvider(),
            "google": GoogleProvider(),
            "deepseek": DeepSeekProvider(),
            "mistral": MistralProvider(),
            "ollama": OllamaProvider(),   # Provider locale on-premise — zero cloud
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
                    loop.close()
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
                    loop.close()
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
            db_url = os.environ.get(
                "DATABASE_URL",
                "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable",
            )
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

    def generate_agent_turn_sync(
        self,
        provider: str,
        model: str,
        messages: list[dict],
        tools: list[dict],
        max_tokens: int = 4096,
        system_text: str = "",
    ) -> ProviderResult:
        """Versione sincrona di generate_agent_turn, sicura da chiamare da thread gRPC.

        Fallback dinamico: provider preferito disabilitato o non compatibile con tool_use
        → cerca nella `_provider_fallback_chain()` (DB-driven) il prossimo abilitato.
        Niente modelli hardcoded: i default model vengono da `nexus_provider_default_model`.
        """
        effective_provider = provider
        effective_model = model

        # Google non supporta tool_use nativo: cerca il primo provider della chain compatibile
        if provider == "google":
            chain = self._provider_fallback_chain(exclude=provider)
            chosen = chain[0] if chain else None
            if chosen:
                fb_model = self._default_model_or_none(chosen) or model
                logger.warning(
                    "Google provider non supporta tool_use nativo, fallback a %s/%s",
                    chosen, fb_model
                )
                effective_provider = chosen
                effective_model = fb_model
            else:
                return ProviderResult(
                    provider=provider, model=model,
                    content="[Nessun provider compatibile con tool_use disponibile per fallback da Google]",
                    metadata={"error": "no_fallback_available", "stop_reason": "error"},
                )

        # Se il provider effettivo e' disabilitato, cerca un fallback tra quelli abilitati
        if not self.is_enabled(effective_provider):
            chain = self._provider_fallback_chain(exclude=effective_provider)
            if chain:
                fb_provider = chain[0]
                fb_model = self._default_model_or_none(fb_provider) or model
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
                def _run():
                    loop = asyncio.new_event_loop()
                    asyncio.set_event_loop(loop)
                    try:
                        return loop.run_until_complete(
                            prov.generate_agent_turn(prov_model, messages, tools, max_tokens, system_text=system_text)
                        )
                    finally:
                        loop.close()
                with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                    return pool.submit(_run).result(timeout=90)
            except Exception as exc:
                logger.error("Agent turn failed for %s/%s: %s", prov_name, prov_model, exc)
                return ProviderResult(
                    provider=prov_name, model=prov_model,
                    content=f"[Error: {exc}]",
                    metadata={"error": str(exc), "stop_reason": "error"},
                )

        result = _run_agent_turn(effective_provider, effective_model)
        usage = (result.metadata or {}).get("usage")
        _record_usage(
            result.provider,
            result.model,
            usage if isinstance(usage, dict) else None,
            {"feature": "neural.GenerateAgentTurn"},
        )

        # Fallback a cascata: se il provider fallisce per errori retriable (billing/quota/errore),
        # proviamo i provider successivi nella chain dinamica (DB-driven, niente hardcoded).
        _RETRIABLE_STOPS = {"billing_error", "rate_limit", "overloaded", "provider_error", "error"}
        if result.metadata.get("stop_reason") in _RETRIABLE_STOPS or result.content.startswith("[Error:"):
            for fb_prov in self._provider_fallback_chain(exclude=effective_provider):
                fb_model = self._default_model_or_none(fb_prov)
                if fb_model is None:
                    logger.warning(
                        "Skip fallback %s: nessun default model in nexus_provider_default_model",
                        fb_prov,
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
                    {"feature": "neural.GenerateAgentTurn", "fallback": True},
                )
                if not (fb_result.metadata.get("stop_reason") in _RETRIABLE_STOPS or fb_result.content.startswith("[Error:")):
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
