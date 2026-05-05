"""LLM-based agentic intent classifier with in-memory TTL cache.

Affronta il limite del classifier keyword-based (`SemanticRouter.classify_intent`):
le richieste in linguaggio naturale per task multi-step (es. "imposta un utente
admin per l'applicazione") cadono su `intent=chat` e vengono routate a modelli
leggeri inadeguati per orchestrare tool call.

Questo classifier usa un LLM piccolo e veloce (default: gemini-2.5-flash) per
produrre un output JSON strutturato che include:
  - intent: come SemanticRouter ma scelto da LLM
  - agentic_score (0..1): probabilità che il task richieda tool use multi-step
  - requires_tools (bool): hint per la UI / routing
  - complexity (low/medium/high): per scegliere tier modello
  - confidence (0..1): fiducia nella classificazione

Cache in-memory con TTL 24h (key=sha256(message[:1000])) per evitare spese
ripetute su prompt identici (es. retry, copia-incolla). Quando il processo
riparte la cache si svuota — accettabile, le entry vengono ripopolate.

Fallback: se LLM fallisce o produce JSON non valido, ritorna un risultato
neutrale con `fallback_used=true` e confidence bassa, così il chiamante può
decidere se usare la classificazione keyword.
"""
from __future__ import annotations

import asyncio
import hashlib
import json
import logging
import re
import time
from dataclasses import dataclass, asdict
from typing import Optional

logger = logging.getLogger(__name__)

# ── Default di sicurezza ────────────────────────────────────────────────────
# Valori usati SOLO se il DB e' irraggiungibile o se la chiave specifica manca.
# In condizioni normali tutti questi sono letti da settings.routing.* (mig 0111)
# tramite _load_classifier_config() con cache TTL 60s.

DEFAULT_CACHE_TTL_SECONDS = 24 * 60 * 60  # 24 ore
DEFAULT_CACHE_MAX_ENTRIES = 10_000
DEFAULT_CLASSIFIER_PROVIDER = "google"
DEFAULT_CLASSIFIER_MODEL = "gemini-2.5-flash"
DEFAULT_LLM_TIMEOUT_SECONDS = 5.0

# TTL della cache di configurazione (legge da settings ogni 60s)
_CONFIG_CACHE_TTL_SECONDS = 60.0

# Intent ammessi (devono coincidere con quelli in nexus_routing_matrix).
ALLOWED_INTENTS = {
    "chat", "debug", "fix", "refactor", "test", "docs",
    "architecture", "file_ops", "system_admin",
}

ALLOWED_COMPLEXITY = {"low", "medium", "high"}


# ── Loader configurazione da settings DB (mig 0111) ─────────────────────────
# Cache process-locale con TTL 60s, allineata al pattern Rust di RoutingMatrixCache.

_CONFIG_CACHE: dict[str, tuple[float, str]] = {}  # key -> (expiry, value)
_CONFIG_LOCK = asyncio.Lock()

# Chiavi DB lette dalla tabella settings con prefisso 'routing.'
_CONFIG_KEYS = [
    "routing.classifier_provider",
    "routing.classifier_model",
    "routing.classifier_cache_ttl_seconds",
    "routing.classifier_cache_max_entries",
    "routing.llm_classifier_timeout_seconds",
]


async def _load_classifier_config() -> dict[str, str]:
    """Carica le settings.routing.classifier_* dal DB con cache 60s.

    In caso di DB irraggiungibile, ritorna i default (logging WARN).
    Idempotente: chiamabile in concorrenza grazie al lock asyncio.
    """
    import time
    import os

    async with _CONFIG_LOCK:
        now = time.monotonic()
        # Cache hit: tutte le chiavi presenti e non scadute
        if all(
            k in _CONFIG_CACHE and _CONFIG_CACHE[k][0] > now
            for k in _CONFIG_KEYS
        ):
            return {k: _CONFIG_CACHE[k][1] for k in _CONFIG_KEYS}

        # Miss: query DB
        try:
            import psycopg2
            db_url = os.environ.get("DATABASE_URL", "")
            if not db_url:
                logger.warning("classifier config: DATABASE_URL not set, uso defaults")
                return _classifier_defaults()
            conn = psycopg2.connect(db_url)
            cur = conn.cursor()
            cur.execute(
                "SELECT key, value FROM settings WHERE key = ANY(%s)",
                ([k for k in _CONFIG_KEYS],),
            )
            rows = cur.fetchall()
            cur.close()
            conn.close()
            result = {k: v for (k, v) in rows}
            # Salva in cache + completa con default le chiavi mancanti
            expiry = now + _CONFIG_CACHE_TTL_SECONDS
            for k in _CONFIG_KEYS:
                v = result.get(k)
                if v is None:
                    v = _classifier_defaults().get(k, "")
                    logger.warning("classifier config: chiave %s mancante in DB, uso default '%s'", k, v)
                _CONFIG_CACHE[k] = (expiry, v)
            return {k: _CONFIG_CACHE[k][1] for k in _CONFIG_KEYS}
        except Exception as exc:  # noqa: BLE001
            logger.warning("classifier config load fallita (%s), uso defaults", exc)
            return _classifier_defaults()


def _classifier_defaults() -> dict[str, str]:
    return {
        "routing.classifier_provider": DEFAULT_CLASSIFIER_PROVIDER,
        "routing.classifier_model": DEFAULT_CLASSIFIER_MODEL,
        "routing.classifier_cache_ttl_seconds": str(DEFAULT_CACHE_TTL_SECONDS),
        "routing.classifier_cache_max_entries": str(DEFAULT_CACHE_MAX_ENTRIES),
        "routing.llm_classifier_timeout_seconds": str(DEFAULT_LLM_TIMEOUT_SECONDS),
    }


# ── Schema risultato ────────────────────────────────────────────────────────

@dataclass
class AgenticIntent:
    intent: str
    agentic_score: float
    requires_tools: bool
    complexity: str
    confidence: float
    model_used: str
    cached: bool = False
    fallback_used: bool = False

    def to_dict(self) -> dict:
        return asdict(self)


# ── Cache TTL in-memory ─────────────────────────────────────────────────────

class _TTLCache:
    """Cache thread-safe LRU + TTL minima. Niente dipendenze esterne."""

    def __init__(self, max_entries: int, ttl_seconds: int):
        self._max = max_entries
        self._ttl = ttl_seconds
        self._store: dict[str, tuple[float, dict]] = {}
        self._lock = asyncio.Lock()

    async def get(self, key: str) -> Optional[dict]:
        async with self._lock:
            entry = self._store.get(key)
            if entry is None:
                return None
            expiry, value = entry
            if time.time() > expiry:
                del self._store[key]
                return None
            return value

    async def put(self, key: str, value: dict) -> None:
        async with self._lock:
            if len(self._store) >= self._max:
                # Eviction: rimuovi 10% delle entry piu' vecchie (poor-man LRU)
                to_evict = max(1, self._max // 10)
                sorted_items = sorted(self._store.items(), key=lambda kv: kv[1][0])
                for k, _ in sorted_items[:to_evict]:
                    del self._store[k]
            self._store[key] = (time.time() + self._ttl, value)

    async def stats(self) -> dict:
        async with self._lock:
            return {
                "entries": len(self._store),
                "max_entries": self._max,
                "ttl_seconds": self._ttl,
            }


# ── Prompt template per l'LLM ───────────────────────────────────────────────

_CLASSIFIER_PROMPT = """You are an intent classifier for a coding assistant ("Nexus").
Classify the following user message and return ONLY a valid JSON object, no markdown, no explanation.

Message:
\"\"\"{message}\"\"\"

Schema (return EXACTLY these keys, all required):
{{
  "intent": one of ["chat","debug","fix","refactor","test","docs","architecture","file_ops","system_admin"],
  "agentic_score": float 0.0..1.0,
  "requires_tools": true or false,
  "complexity": one of ["low","medium","high"],
  "confidence": float 0.0..1.0
}}

Definitions:
- intent: primary task category
  * "chat" = conversational, no codebase action expected
  * "debug" = analyze stack traces, find root cause
  * "fix" = repair a known bug
  * "refactor" = restructure code without behavior change
  * "test" = write/improve tests
  * "docs" = write/improve documentation
  * "architecture" = high-level design or migration plan
  * "file_ops" = create/delete/move files (no code logic)
  * "system_admin" = configure services, users, deployments, infrastructure
- agentic_score: 1.0 if the task REQUIRES multiple tool calls (read_file, write_file,
  run_command, etc.); 0.0 if a single text reply suffices.
- requires_tools: true if the assistant must read or modify the codebase / system.
- complexity: "low" = single file or single command; "medium" = a few files or steps;
  "high" = cross-cutting changes, migrations, architectural work.
- confidence: how sure are you of the classification.

Examples:
- "ciao come stai" → {{"intent":"chat","agentic_score":0.0,"requires_tools":false,"complexity":"low","confidence":0.99}}
- "imposta un utente admin per l'applicazione" → {{"intent":"system_admin","agentic_score":0.9,"requires_tools":true,"complexity":"medium","confidence":0.92}}
- "perché la mia funzione torna null?" → {{"intent":"debug","agentic_score":0.7,"requires_tools":true,"complexity":"medium","confidence":0.85}}
- "spiegami il pattern repository" → {{"intent":"chat","agentic_score":0.05,"requires_tools":false,"complexity":"low","confidence":0.95}}

Return ONLY the JSON object."""


# ── Classifier principale ───────────────────────────────────────────────────

class AgenticIntentClassifier:
    """Classifier LLM-based con cache. Riusa un ProviderRegistry esistente.

    Provider/model/timeout sono letti da DB (`settings.routing.classifier_*`,
    mig 0111) con cache 60s. Override esplicito tramite kwargs `provider`/`model`
    salta il lookup DB (utile per testing).
    """

    def __init__(self, provider_registry, fallback_classifier=None,
                 provider: Optional[str] = None,
                 model: Optional[str] = None):
        self._providers = provider_registry
        self._fallback = fallback_classifier  # SemanticRouter o None
        # Se passati esplicitamente, hanno precedenza assoluta (no DB lookup).
        # Altrimenti lazy-load dal DB al primo classify() / _ensure_config().
        self._explicit_provider = provider
        self._explicit_model = model
        self._provider: Optional[str] = provider
        self._model: Optional[str] = model
        # Cache costruita lazy alla prima classify() perche' la sua dimensione
        # dipende dalle settings DB.
        self._cache: Optional[_TTLCache] = None

    async def _ensure_config(self) -> None:
        """Garantisce che provider/model/cache siano popolati dalle settings DB
        o dai default. Idempotente: dopo la prima chiamata e' un no-op fino al
        TTL della cache config (60s)."""
        if self._explicit_provider and self._explicit_model and self._cache is not None:
            return
        cfg = await _load_classifier_config()
        if not self._explicit_provider:
            self._provider = cfg["routing.classifier_provider"]
        if not self._explicit_model:
            self._model = cfg["routing.classifier_model"]
        if self._cache is None:
            try:
                ttl = int(cfg["routing.classifier_cache_ttl_seconds"])
                max_entries = int(cfg["routing.classifier_cache_max_entries"])
            except (ValueError, KeyError):
                ttl, max_entries = DEFAULT_CACHE_TTL_SECONDS, DEFAULT_CACHE_MAX_ENTRIES
            self._cache = _TTLCache(max_entries, ttl)
        # Aggiorna timeout LLM (consultato in classify())
        try:
            self._llm_timeout = float(cfg["routing.llm_classifier_timeout_seconds"])
        except (ValueError, KeyError):
            self._llm_timeout = DEFAULT_LLM_TIMEOUT_SECONDS

    @staticmethod
    def _cache_key(message: str) -> str:
        head = message.strip()[:1000]
        return hashlib.sha256(head.encode("utf-8")).hexdigest()

    @staticmethod
    def _validate_parsed(parsed: dict) -> Optional[AgenticIntent]:
        try:
            intent = str(parsed["intent"]).strip().lower()
            if intent not in ALLOWED_INTENTS:
                return None
            agentic_score = float(parsed["agentic_score"])
            requires_tools = bool(parsed["requires_tools"])
            complexity = str(parsed["complexity"]).strip().lower()
            if complexity not in ALLOWED_COMPLEXITY:
                return None
            confidence = float(parsed["confidence"])
            # Clamp scores in [0, 1]
            agentic_score = max(0.0, min(1.0, agentic_score))
            confidence = max(0.0, min(1.0, confidence))
            return AgenticIntent(
                intent=intent,
                agentic_score=agentic_score,
                requires_tools=requires_tools,
                complexity=complexity,
                confidence=confidence,
                model_used="",  # riempito dopo
            )
        except (KeyError, ValueError, TypeError) as exc:
            logger.warning("classifier: parsed JSON malformed: %s", exc)
            return None

    @staticmethod
    def _extract_json(content: str) -> Optional[dict]:
        """Estrae il primo oggetto JSON dal contenuto LLM (anche se circondato
        da markdown fences o testo). Robusto a piccole imperfezioni del modello."""
        if not content:
            return None
        # Strip markdown code fences
        content = re.sub(r"^```(?:json)?\s*", "", content.strip())
        content = re.sub(r"\s*```\s*$", "", content)
        # Trova il primo { ... } bilanciato
        try:
            return json.loads(content)
        except json.JSONDecodeError:
            pass
        # Fallback: regex per il primo blocco { ... }
        match = re.search(r"\{[^{}]*(?:\{[^{}]*\}[^{}]*)*\}", content, re.DOTALL)
        if match:
            try:
                return json.loads(match.group(0))
            except json.JSONDecodeError:
                return None
        return None

    def _fallback_result(self, message: str, reason: str) -> AgenticIntent:
        """Costruisce un risultato di fallback usando il classifier keyword (se
        disponibile) e marcando `fallback_used=True`."""
        intent = "chat"
        confidence = 0.30
        if self._fallback is not None:
            try:
                kw_result = self._fallback.classify_intent(message)
                intent = str(kw_result.get("intent", "chat"))
                confidence = float(kw_result.get("confidence", 0.30))
            except Exception as exc:  # noqa: BLE001
                logger.warning("classifier: fallback keyword failed: %s", exc)
        # Heuristic minima per agentic_score: se intent != chat, alza un po'
        agentic_score = 0.6 if intent != "chat" else 0.2
        return AgenticIntent(
            intent=intent,
            agentic_score=agentic_score,
            requires_tools=intent != "chat",
            complexity="medium",
            confidence=confidence * 0.5,  # confidence ridotta perche' fallback
            model_used=f"fallback:{reason}",
            cached=False,
            fallback_used=True,
        )

    async def classify(self, message: str) -> AgenticIntent:
        """Punto di ingresso: cache-first → LLM → fallback keyword.
        Carica config da DB (mig 0111) al primo call con cache 60s."""
        if not message or not message.strip():
            return self._fallback_result(message, "empty_message")

        # Lazy load config (provider/model/cache size/timeout) da settings DB.
        await self._ensure_config()
        timeout_s = getattr(self, "_llm_timeout", DEFAULT_LLM_TIMEOUT_SECONDS)

        key = self._cache_key(message)
        cached = await self._cache.get(key)
        if cached is not None:
            result = AgenticIntent(**cached)
            result.cached = True
            return result

        # Chiamata LLM con timeout configurabile
        prompt = _CLASSIFIER_PROMPT.format(message=message[:2000])
        try:
            llm_call = self._providers.generate_completion_async(
                self._provider, self._model, prompt
            )
            llm_result = await asyncio.wait_for(llm_call, timeout=timeout_s)
        except asyncio.TimeoutError:
            logger.warning("classifier: LLM timeout (%ss) on key=%s", timeout_s, key[:8])
            return self._fallback_result(message, "timeout")
        except Exception as exc:  # noqa: BLE001
            logger.warning("classifier: LLM failed: %s", exc)
            return self._fallback_result(message, f"llm_error:{type(exc).__name__}")

        parsed = self._extract_json(getattr(llm_result, "content", ""))
        if parsed is None:
            logger.warning("classifier: cannot parse JSON from LLM (key=%s)", key[:8])
            return self._fallback_result(message, "json_parse")

        validated = self._validate_parsed(parsed)
        if validated is None:
            return self._fallback_result(message, "json_validation")

        validated.model_used = getattr(llm_result, "model", self._model)
        # Memorizza in cache (senza il flag cached=true, viene messo al recupero)
        await self._cache.put(key, asdict(validated))
        return validated

    async def stats(self) -> dict:
        """Esposto per debug/monitoring."""
        cache_stats = await self._cache.stats()
        return {
            "cache": cache_stats,
            "provider": self._provider,
            "model": self._model,
        }
