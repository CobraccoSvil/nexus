"""Semantic router with embedding-based intent classification."""
from __future__ import annotations

import logging
from dataclasses import dataclass
import urllib.parse


logger = logging.getLogger(__name__)

# Intent exemplars for embedding-based classification
# NOTA Fase A consolidamento (vedi piano `questo-lo-stesso-proud-blossom.md`):
#
# `_RISKY_KEYWORDS` e `_is_risky_task` sono stati RIMOSSI da Python — la fonte
# autoritativa e' ora `is_risky_task()` in `crates/mcp-core/src/orchestrator.rs`.
# Tutti i caller di Python che avevano bisogno di valutare se un task fosse
# rischioso devono delegare al routing Rust via `/api/internal/routing/decide`,
# che applica gia' l'override automatico mode = "approfondita" quando rileva
# verbi distruttivi.
#
# Mantenere una sola fonte evita il drift gia' osservato in produzione (es.
# Rust riconosceva intent `debug` su stack trace, Python no → modelli diversi
# per stesso input).


@dataclass(slots=True)
class RoutingDecision:
    provider: str
    model: str
    rationale: str
    confidence: float = 0.0


# ── Thin client per /api/internal/routing/decide ────────────────────────────
# Singleton inizializzato lazily dal modulo. Cache TTL 30s per evitare
# round-trip HTTP ripetuti durante un singolo turno agente.

class _RoutingClient:
    """Client HTTP minimale verso `mcp-core /api/internal/routing/decide`.

    Cache per (message, behavior_mode) con TTL 30s. Fallback safe se il
    Rust non risponde (mai blocca il routing del brain).
    """
    _DEFAULT_TIMEOUT_S = 1.5
    _CACHE_TTL_S = 30.0

    def __init__(self, base_url: str | None = None) -> None:
        import os
        from brain.utils.settings_db import get_setting
        self._base = (
            base_url
            or os.getenv("MCP_CORE_URL")
            or get_setting("mcp_core_url", "http://127.0.0.1:4000")
        ).rstrip("/")
        self._cache: dict[tuple[str, str], tuple[float, RoutingDecision]] = {}
        # Cache del set provider in cooldown (ADR 0020), TTL condiviso.
        self._cooldown_cache: tuple[float, set[str]] | None = None

    def decide(self, *, message: str, behavior_mode: str) -> RoutingDecision:
        import time
        import urllib.request
        import urllib.error
        import json as _json
        key = (message[:512], behavior_mode)  # cap message length nella key
        now = time.monotonic()
        entry = self._cache.get(key)
        if entry and now - entry[0] < self._CACHE_TTL_S:
            return entry[1]
        body = _json.dumps({
            "message": message,
            "behavior_mode": behavior_mode,
        }).encode("utf-8")
        url = f"{self._base}/api/internal/routing/decide"
        req = urllib.request.Request(url, data=body, headers={"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=self._DEFAULT_TIMEOUT_S) as resp:
                payload = _json.loads(resp.read().decode("utf-8"))
            # Provider/model devono SEMPRE essere presenti nel payload del Rust
            # router; se mancano, si tratta di una risposta malformata —
            # ritorniamo la sentinella di errore visibile invece che un default
            # safe nascosto (regola G CLAUDE.md).
            payload_provider = payload.get("provider")
            payload_model = payload.get("model")
            if not payload_provider or not payload_model:
                logger.error(
                    "Routing: payload Rust malformato (provider=%s model=%s) — sentinella",
                    payload_provider, payload_model,
                )
                return RoutingDecision(
                    provider="__router_unavailable__",
                    model="__router_unavailable__",
                    rationale="Rust routing payload malformato (provider/model mancanti)",
                    confidence=0.0,
                )
            decision = RoutingDecision(
                provider=payload_provider,
                model=payload_model,
                rationale=payload.get("rationale", "Rust routing (no rationale)"),
                confidence=0.92,
            )
            self._cache[key] = (now, decision)
            return decision
        except urllib.error.HTTPError as he:
            # 503 Service Unavailable = nessun provider capable (tutti in cooldown).
            # Il body contiene comunque la decisione con `no_capable_provider=true`,
            # propaghiamo come decisione "speciale" che il chiamante a monte deve
            # gestire fermando il flusso agente.
            if he.code == 503:
                try:
                    body = _json.loads(he.read().decode("utf-8"))
                    rationale = body.get("rationale", "Nessun provider disponibile")
                    in_cd = body.get("providers_in_cooldown", [])
                    logger.error(
                        "Routing 503: nessun provider disponibile (in cooldown: %s)",
                        ",".join(in_cd),
                    )
                    return RoutingDecision(
                        provider="__no_capable_provider__",
                        model="__no_capable_provider__",
                        rationale=f"NESSUN PROVIDER DISPONIBILE: {rationale}",
                        confidence=0.0,
                    )
                except Exception:
                    pass
            # Errore HTTP non-503 (500, timeout, malformed): sentinella di
            # errore visibile invece di degradare silenziosamente a un modello
            # arbitrario (regola G CLAUDE.md: niente magic fallback).
            logger.warning("Routing HTTP error: %s", he)
            return RoutingDecision(
                provider="__router_unavailable__",
                model="__router_unavailable__",
                rationale=f"Rust router HTTP {he.code}",
                confidence=0.0,
            )
        except (urllib.error.URLError, TimeoutError, _json.JSONDecodeError, OSError) as e:
            # Rust router irraggiungibile (connection refused, timeout, JSON
            # parse error): sentinella visibile. Il chiamante DEVE intercettare
            # __router_unavailable__ e fermare il flusso, non proseguire con
            # un modello potenzialmente sbagliato. Coerente con il pattern gia'
            # usato per __no_capable_provider__.
            logger.error(
                "Routing /api/internal/routing/decide non raggiungibile (%s) — sentinella __router_unavailable__",
                e,
            )
            return RoutingDecision(
                provider="__router_unavailable__",
                model="__router_unavailable__",
                rationale=f"Rust router non disponibile: {e}",
                confidence=0.0,
            )

    def purpose_model(self, *, purpose: str) -> RoutingDecision:
        """Resolve (provider, model) for an internal purpose via Rust endpoint.

        Purpose models are DB-driven (nexus_purpose_model) and configurable from admin UI.
        """
        import time
        import urllib.request
        import urllib.error
        import json as _json

        p = (purpose or "").strip()
        if not p:
            return RoutingDecision(
                provider="__router_unavailable__",
                model="__router_unavailable__",
                rationale="purpose vuoto",
                confidence=0.0,
            )
        # Cache TTL condivisa: chiave purpose + behavior_mode fittizio
        key = (f"purpose:{p}", "purpose")
        now = time.monotonic()
        entry = self._cache.get(key)
        if entry and now - entry[0] < self._CACHE_TTL_S:
            return entry[1]

        url = f"{self._base}/api/internal/routing/purpose?purpose={urllib.parse.quote(p)}"
        req = urllib.request.Request(url, headers={"Accept": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=self._DEFAULT_TIMEOUT_S) as resp:
                payload = _json.loads(resp.read().decode("utf-8"))
            payload_provider = payload.get("provider")
            payload_model = payload.get("model")
            if not payload_provider or not payload_model:
                return RoutingDecision(
                    provider="__router_unavailable__",
                    model="__router_unavailable__",
                    rationale="purpose payload malformato (provider/model mancanti)",
                    confidence=0.0,
                )
            decision = RoutingDecision(
                provider=payload_provider,
                model=payload_model,
                rationale=payload.get("rationale", "purpose_model"),
                confidence=0.95,
            )
            self._cache[key] = (now, decision)
            return decision
        except urllib.error.HTTPError as he:
            # 503 = il purpose risolve su un provider in cooldown e non c'e'
            # alternativa capable (ADR 0020). Il gate ce lo dice esplicitamente:
            # propaghiamo __no_capable_provider__ cosi' il chiamante (escalation/
            # loop_fallback) SALTA il fallback invece di ritentare un provider
            # morto. Distinto da __router_unavailable__ (Rust irraggiungibile).
            if he.code == 503:
                try:
                    body = _json.loads(he.read().decode("utf-8"))
                    rationale = body.get("rationale", "purpose in cooldown")
                except Exception:
                    rationale = "purpose in cooldown"
                logger.warning(
                    "Routing purpose 503: nessun provider disponibile per '%s' (%s)",
                    p, rationale,
                )
                return RoutingDecision(
                    provider="__no_capable_provider__",
                    model="__no_capable_provider__",
                    rationale=f"NESSUN PROVIDER DISPONIBILE (purpose): {rationale}",
                    confidence=0.0,
                )
            logger.warning("Routing purpose HTTP error: %s", he)
            return RoutingDecision(
                provider="__router_unavailable__",
                model="__router_unavailable__",
                rationale=f"purpose HTTP {he.code}",
                confidence=0.0,
            )
        except (urllib.error.URLError, TimeoutError, _json.JSONDecodeError, OSError) as e:
            logger.error("Routing purpose non raggiungibile (%s)", e)
            return RoutingDecision(
                provider="__router_unavailable__",
                model="__router_unavailable__",
                rationale=f"purpose non disponibile: {e}",
                confidence=0.0,
            )

    def cooldown_providers(self) -> set[str] | None:
        """Provider attualmente in cooldown secondo il gate Rust (ADR 0020).

        Fonte di verita' UNICA a runtime per il cooldown: il gate Rust accumula
        sia i cooldown osservati direttamente sia quelli riportati dal brain via
        `provider-error` (cooldown_bridge). Consultando questo endpoint il brain
        non duplica il ragionamento sul cooldown (regola H) e salta in
        fallback/escalation gli stessi provider che il gate considera morti.

        Ritorna l'insieme dei nomi provider (lowercase) in cooldown, oppure
        `None` se il gate e' irraggiungibile (il chiamante decide se ripiegare
        sulla propria vista locale come degrado, non come fonte primaria).
        Cache TTL condivisa 30s per non martellare l'endpoint nel turno.
        """
        import time
        import urllib.request
        import urllib.error
        import json as _json

        now = time.monotonic()
        entry = self._cooldown_cache
        if entry is not None and now - entry[0] < self._CACHE_TTL_S:
            return entry[1]

        url = f"{self._base}/api/internal/routing/cooldown"
        req = urllib.request.Request(url, headers={"Accept": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=self._DEFAULT_TIMEOUT_S) as resp:
                payload = _json.loads(resp.read().decode("utf-8"))
            providers = {
                str(e.get("provider", "")).strip().lower()
                for e in (payload.get("providers") or [])
                if str(e.get("provider", "")).strip()
            }
            self._cooldown_cache = (now, providers)
            return providers
        except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError, _json.JSONDecodeError, OSError) as e:
            logger.warning("Routing cooldown set non raggiungibile (%s)", e)
            return None


_ROUTING_CLIENT_INSTANCE: _RoutingClient | None = None


def _routing_client_singleton() -> _RoutingClient:
    global _ROUTING_CLIENT_INSTANCE
    if _ROUTING_CLIENT_INSTANCE is None:
        _ROUTING_CLIENT_INSTANCE = _RoutingClient()
    return _ROUTING_CLIENT_INSTANCE


# ── Smart routing vision (allegati image/*) ────────────────────────────────
# Quando il messaggio utente porta allegati immagine, se il modello scelto
# dalla routing matrix NON e vision-capable preferiamo il vision-capable
# piu economico configurato in ai_price_catalog. Niente fallback hardcoded:
# se la lookup fallisce o nessun modello vision e disponibile, la decisione
# originale viene mantenuta con un marker nel rationale per la UI.

def _model_has_vision_capability(provider: str, model: str) -> bool | None:
    """Ritorna True/False se sappiamo dal catalog, None se DB irraggiungibile."""
    if not provider or not model:
        return False
    try:
        import os
        import psycopg2
        database_url = os.environ.get("DATABASE_URL")
        if not database_url:
            return None
        conn = psycopg2.connect(database_url, connect_timeout=2)
        try:
            cur = conn.cursor()
            cur.execute(
                "SELECT supports_vision "
                "FROM ai_price_catalog "
                "WHERE provider = %s AND model = %s AND is_enabled = true LIMIT 1",
                (provider, model),
            )
            row = cur.fetchone()
            if row is None:
                return False
            return bool(row[0])
        finally:
            conn.close()
    except Exception as exc:
        logger.warning("vision_routing: catalog lookup fallita per %s/%s: %s", provider, model, exc)
        return None


def _select_cheapest_vision_model() -> tuple[str, str] | None:
    """Ritorna (provider, model) del piu economico vision-capable in catalog."""
    try:
        import os
        import psycopg2
        database_url = os.environ.get("DATABASE_URL")
        if not database_url:
            return None
        conn = psycopg2.connect(database_url, connect_timeout=2)
        try:
            cur = conn.cursor()
            cur.execute(
                "SELECT provider, model FROM ai_price_catalog "
                "WHERE supports_vision = true AND is_enabled = true "
                "ORDER BY input_cost_per_million_tokens ASC NULLS LAST LIMIT 1"
            )
            row = cur.fetchone()
            if row is None:
                return None
            return (str(row[0]), str(row[1]))
        finally:
            conn.close()
    except Exception as exc:
        logger.warning("vision_routing: select_cheapest fallito: %s", exc)
        return None


def _apply_vision_override(decision: "RoutingDecision") -> "RoutingDecision":
    """Sostituisce la decisione se il modello scelto non ha vision.

    Aggiunge nel rationale il marker vision_routing per audit. Se NESSUN
    vision-capable e disponibile, mantiene la decisione e marca
    vision_unavailable=true cosi il caller puo avvisare l utente.
    """
    cap = _model_has_vision_capability(decision.provider, decision.model)
    if cap is True:
        return decision
    chosen = _select_cheapest_vision_model()
    if chosen is None:
        logger.warning(
            "vision_routing: nessun modello vision-capable disponibile; mantengo %s/%s ma marco vision_unavailable",
            decision.provider, decision.model,
        )
        return RoutingDecision(
            provider=decision.provider,
            model=decision.model,
            rationale=(decision.rationale or "") + " | vision_unavailable=true",
            confidence=decision.confidence,
        )
    new_provider, new_model = chosen
    if new_provider == decision.provider and new_model == decision.model:
        return decision
    logger.info(
        "vision_routing: override %s/%s -> %s/%s (motivo: allegati image/* presenti)",
        decision.provider, decision.model, new_provider, new_model,
    )
    return RoutingDecision(
        provider=new_provider,
        model=new_model,
        rationale=(
            (decision.rationale or "") +
            f" | vision_routing: override {decision.provider}/{decision.model} -> {new_provider}/{new_model}"
        ),
        confidence=max(decision.confidence, 0.8),
    )


class SemanticRouter:
    """Routes requests to the best model based on intent classification."""

    # SemanticRouter ora e' un thin-client puro verso il routing Rust
    # (`route_model` -> /api/internal/routing/decide). La classificazione
    # dell'intent NON e' piu' qui: e' solo semantica (classifier LLM in
    # brain/router/agentic_classifier.py). Rimossi _classify_by_keywords,
    # _classify_by_embedding, _init_intent_vectors e _INTENT_EXEMPLARS:
    # niente piu' interpretazione del testo via confronto di stringhe.
    def __init__(self) -> None:
        pass

    def route_model(
        self,
        intent: str,
        token_budget: int,
        behavior_mode: str = "bilanciata",
        message: str | None = None,
        has_image_attachments: bool = False,
    ) -> RoutingDecision:
        """Decide provider/model consultando l'orchestrator Rust via REST.

        Thin client: nessuna logica locale di routing. La fonte autoritativa
        e' `POST /api/internal/routing/decide` esposto da mcp-core (vedi
        `crates/mcp-core/src/internal_routing.rs`).

        Niente fallback hardcoded (CLAUDE.md §G): se il Rust router e'
        irraggiungibile o ritorna 503 / payload malformato, viene restituita
        una sentinella visibile `provider="__router_unavailable__"` (o
        `"__no_capable_provider__"` per 503) che il chiamante DEVE intercettare
        per fermare il flusso, non degradare silenziosamente a un modello
        arbitrario.

        Cache locale: i risultati vengono cached per 30s in base al messaggio
        + behavior_mode, per evitare RTT su decisioni ripetute durante un
        singolo turno agente.
        """
        decision = _routing_client_singleton().decide(
            message=message or "",
            behavior_mode=behavior_mode,
        )
        if has_image_attachments and not decision.provider.startswith("__"):
            return _apply_vision_override(decision)
        return decision
