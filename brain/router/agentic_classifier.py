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
from dataclasses import dataclass, asdict, field
from typing import Optional

logger = logging.getLogger(__name__)


class ClassifierConfigUnavailable(Exception):
    """Sollevata quando provider/model del classifier agentico non possono
    essere risolti dal DB (DB irraggiungibile o chiavi settings.routing.*
    assenti).

    Regola G di CLAUDE.md: vietato silently-fallback a un modello hardcoded.
    Il caller deve degradare al classifier keyword-based o ritornare errore
    visibile, non scegliere un modello a caso.
    """
    pass


# ── Parametri operativi (NON model name) ────────────────────────────────────
# Questi sono solo dimensioni di cache e timeout. Niente nomi di modello/provider:
# quelli devono SEMPRE arrivare dal DB (settings.routing.classifier_provider/model).

DEFAULT_CACHE_TTL_SECONDS = 24 * 60 * 60  # 24 ore
DEFAULT_CACHE_MAX_ENTRIES = 10_000
DEFAULT_LLM_TIMEOUT_SECONDS = 5.0

# TTL della cache di configurazione (legge da settings ogni 60s)
_CONFIG_CACHE_TTL_SECONDS = 60.0

# Intent ammessi (devono coincidere con quelli in nexus_routing_matrix).
# "code_read" aggiunto per distinguere la LETTURA di file/codice dalla
# PRODUZIONE di documentazione ("docs"). Senza questo intent separato,
# "leggi il file X" e "elenca i file" cadevano su `docs` e il RAG inline
# (BP7 in nodes.py _RAG_INTENTS) non si attivava mai.
ALLOWED_INTENTS = {
    "chat", "debug", "fix", "refactor", "test", "docs",
    "architecture", "file_ops", "system_admin", "code_read",
}

ALLOWED_COMPLEXITY = {"low", "medium", "high"}


# ── Loader configurazione da settings DB (mig 0111) ─────────────────────────
# Cache process-locale con TTL 60s, allineata al pattern Rust di RoutingMatrixCache.

_CONFIG_CACHE: dict[str, tuple[float, str]] = {}  # key -> (expiry, value)
_CONFIG_LOCK = asyncio.Lock()

# Chiavi DB lette dalla tabella settings con prefisso 'routing.'
# Le soglie ambiguity_* (mig 0132) sono parametri operativi: se mancano dal DB
# si usano i default tecnici da _classifier_operational_defaults().
_CONFIG_KEYS = [
    "routing.classifier_provider",
    "routing.classifier_model",
    "routing.classifier_cache_ttl_seconds",
    "routing.classifier_cache_max_entries",
    "routing.llm_classifier_timeout_seconds",
    "routing.ambiguity_min_confidence",   # mig 0132
    "routing.ambiguity_min_margin",       # mig 0132
]


async def _load_classifier_config() -> dict[str, str]:
    """Carica le settings.routing.classifier_* dal DB con cache 60s.

    Comportamento (regola G di CLAUDE.md):
    - DB OK + tutte le chiavi presenti: ritorna il dict completo
    - DB irraggiungibile o `routing.classifier_provider/model` assenti:
      solleva `ClassifierConfigUnavailable` con messaggio esplicito
    - Solo le chiavi operative (TTL, max_entries, timeout) hanno default
      tecnico ammissibile, NON il modello

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
        db_url = os.environ.get("DATABASE_URL", "")
        if not db_url:
            raise ClassifierConfigUnavailable(
                "DATABASE_URL non impostata: impossibile leggere "
                "settings.routing.classifier_provider/model. "
                "Configurare la variabile d'ambiente."
            )
        try:
            import psycopg2
            conn = psycopg2.connect(db_url)
            cur = conn.cursor()
            cur.execute(
                "SELECT key, value FROM settings WHERE key = ANY(%s)",
                ([k for k in _CONFIG_KEYS],),
            )
            rows = cur.fetchall()
            cur.close()
            conn.close()
        except Exception as exc:  # noqa: BLE001
            raise ClassifierConfigUnavailable(
                f"DB irraggiungibile: {exc}. Verifica Postgres e migrazione 0111."
            ) from exc

        result = {k: v for (k, v) in rows}
        # Provider/model: OBBLIGATORI da DB (regola G). Le costanti operative
        # (TTL, max_entries, timeout) tollerano un fallback tecnico.
        provider = result.get("routing.classifier_provider")
        model = result.get("routing.classifier_model")
        if not provider or not model:
            raise ClassifierConfigUnavailable(
                "settings.routing.classifier_provider/model mancanti nel DB. "
                "Applicare la migrazione 0111 e popolare la tabella `settings`."
            )

        # Operativi (timeout, cache size): default tecnico se mancanti
        op_defaults = _classifier_operational_defaults()
        expiry = now + _CONFIG_CACHE_TTL_SECONDS
        for k in _CONFIG_KEYS:
            v = result.get(k)
            if v is None:
                v = op_defaults.get(k)
                if v is None:
                    # Non dovrebbe mai accadere: provider/model gia' verificati sopra
                    raise ClassifierConfigUnavailable(
                        f"chiave {k} senza default tecnico e assente nel DB"
                    )
                logger.warning(
                    "classifier config: chiave operativa %s mancante in DB, uso default tecnico '%s'",
                    k, v,
                )
            _CONFIG_CACHE[k] = (expiry, v)
        return {k: _CONFIG_CACHE[k][1] for k in _CONFIG_KEYS}


@dataclass
class ClassifierChainEntry:
    """Una entry della chain di provider per il classifier agentico.

    Sorgente autoritativa: tabella `nexus_classifier_provider_chain` (mig 0134).
    """
    provider: str
    model: str
    priority: int


# Cache process-local della chain (TTL 60s, allineato a _CONFIG_CACHE).
_CHAIN_CACHE: list[ClassifierChainEntry] = []
_CHAIN_CACHE_EXPIRY: float = 0.0


async def _load_classifier_chain() -> list[ClassifierChainEntry]:
    """Carica la chain di provider/model per il classifier dalla tabella
    `nexus_classifier_provider_chain` (mig 0134) con cache 60s.

    Comportamento (regola G di CLAUDE.md):
    - Tabella popolata: ritorna entries ordinate per priority DESC
    - Tabella vuota O DB irraggiungibile: ritorna lista vuota
      → il caller usera' la singola entry da `settings.routing.classifier_*`
        come fallback retrocompatibile.

    Niente hardcoded: nessun model name nel codice.
    """
    global _CHAIN_CACHE, _CHAIN_CACHE_EXPIRY
    import time
    import os
    now = time.monotonic()
    if _CHAIN_CACHE and now < _CHAIN_CACHE_EXPIRY:
        return list(_CHAIN_CACHE)  # snapshot immutabile
    db_url = os.environ.get("DATABASE_URL", "")
    if not db_url:
        logger.warning("classifier_chain: DATABASE_URL non set, chain vuota")
        return []
    try:
        import psycopg2  # type: ignore[import-untyped]
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT provider, model_id, priority "
                    "FROM nexus_classifier_provider_chain "
                    "WHERE is_active = TRUE "
                    "ORDER BY priority DESC, provider ASC",
                )
                rows = cur.fetchall()
    except Exception as exc:
        logger.warning("classifier_chain: load fallita (%s), chain vuota", exc)
        return []
    chain = [
        ClassifierChainEntry(provider=p, model=m, priority=int(pr))
        for (p, m, pr) in rows
    ]
    _CHAIN_CACHE = chain
    _CHAIN_CACHE_EXPIRY = now + 60.0
    return list(chain)


def _classifier_operational_defaults() -> dict[str, str]:
    """Default tecnici per i SOLI parametri operativi (TTL, dimensione cache,
    timeout, soglie ambiguity). NON include provider/model: quelli devono
    sempre venire dal DB secondo la regola G di CLAUDE.md."""
    return {
        "routing.classifier_cache_ttl_seconds": str(DEFAULT_CACHE_TTL_SECONDS),
        "routing.classifier_cache_max_entries": str(DEFAULT_CACHE_MAX_ENTRIES),
        "routing.llm_classifier_timeout_seconds": str(DEFAULT_LLM_TIMEOUT_SECONDS),
        "routing.ambiguity_min_confidence": str(DEFAULT_AMBIGUITY_MIN_CONFIDENCE),
        "routing.ambiguity_min_margin": str(DEFAULT_AMBIGUITY_MIN_MARGIN),
    }


# ── Schema risultato ────────────────────────────────────────────────────────

# Default TECNICI delle soglie di disambiguazione (best practice NLU:
# Rasa/Dialogflow/LUIS). Usati SOLO come fallback se il DB non e' disponibile
# o se la chiave `settings.routing.ambiguity_min_*` manca (regola G CLAUDE.md).
# Sorgente autoritativa: `settings.routing.ambiguity_min_confidence/margin` (mig 0132).
# Letti via `_load_classifier_config()` con cache 60s.
DEFAULT_AMBIGUITY_MIN_CONFIDENCE = 0.70
DEFAULT_AMBIGUITY_MIN_MARGIN = 0.15


@dataclass
class IntentCandidate:
    """Intent candidato con confidence individuale (multi-label / disambig)."""
    intent: str
    confidence: float

    def to_dict(self) -> dict:
        return {"intent": self.intent, "confidence": self.confidence}


# ── Slot filling: schema canonico (Livello 4 NLU) ───────────────────────────

# action_verb: cosa l'utente vuole FARE col target
ALLOWED_ACTION_VERBS = {
    "read", "write", "resolve", "analyze",
    "refactor", "configure", "deploy", "delete",
}

# target.type: su cosa agisce l'azione
ALLOWED_TARGET_TYPES = {
    "code", "tests", "config", "service",
    "docs", "data", "infrastructure",
}

# scope: ampiezza del cambiamento
ALLOWED_SCOPES = {"single", "multi_file", "cross_service", "system_wide"}


@dataclass
class ActionSlots:
    """Slot canonici estratti dal task dell'utente.

    Forniscono routing piu' preciso di (intent, behavior_mode):
    una `resolve tests playwright multi_file` chiede esplicitamente un
    modello capable per multi-file edit + debug, evitando il bug di
    routing che mandava "test failure" su modelli light.

    `framework` e' free-form (lower-case): es. "playwright", "pytest",
    "cargo", "jest", "vitest", "npm", "docker". Stringa vuota se non
    inferibile dal messaggio.

    `confidence` 0..1: fiducia complessiva del modello sull'estrazione
    dei 4 slot. Se < 0.6 il caller fa fallback al routing classico
    `(intent, behavior_mode)`.
    """
    action_verb: str = ""
    target_type: str = ""
    framework: str = ""
    scope: str = ""
    confidence: float = 0.0

    def is_complete(self) -> bool:
        """True se action_verb, target_type, scope sono tutti popolati e validi.
        framework e' opzionale (puo' essere stringa vuota = wildcard)."""
        return (
            self.action_verb in ALLOWED_ACTION_VERBS
            and self.target_type in ALLOWED_TARGET_TYPES
            and self.scope in ALLOWED_SCOPES
        )

    def to_dict(self) -> dict:
        return {
            "action_verb": self.action_verb,
            "target_type": self.target_type,
            "framework": self.framework,
            "scope": self.scope,
            "confidence": self.confidence,
        }


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
    # Lista candidati alternativi (top 3 sortati per confidence DESC).
    # Sempre contiene almeno [self.intent] come primo elemento.
    candidates: list[IntentCandidate] = field(default_factory=list)
    # True quando confidence < AMBIGUITY_MIN_CONFIDENCE oppure
    # (top - second) < AMBIGUITY_MIN_MARGIN. L'agente esegutore deve chiedere
    # chiarimenti invece di scegliere arbitrariamente uno degli intent.
    is_ambiguous: bool = False
    # Slot canonici per routing slot-based (Livello 4 NLU).
    # Se `slots.is_complete()` E `slots.confidence >= 0.60`, il caller usa
    # `nexus_routing_slots_matrix` come fonte primaria di routing (piu'
    # specifica della classica (intent, behavior_mode)). Altrimenti fallback
    # gerarchico al routing classico.
    slots: ActionSlots = field(default_factory=ActionSlots)

    def to_dict(self) -> dict:
        d = asdict(self)
        # asdict serializza candidates e slots come dict → ok
        return d


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

_CLASSIFIER_PROMPT = """Intent classifier for a coding assistant. Return ONLY a JSON object, no markdown, no text.

Message: \"\"\"{message}\"\"\"

Schema (all keys required):
{{
"intent": one of ["chat","debug","fix","refactor","test","docs","architecture","file_ops","system_admin","code_read"],
"agentic_score": 0.0..1.0,
"requires_tools": bool,
"complexity": "low"|"medium"|"high",
"confidence": 0.0..1.0,
"candidates": [{{"intent":"...","confidence":0..1}}, up to 3],
"slots": {{
  "action_verb": "read"|"write"|"resolve"|"analyze"|"refactor"|"configure"|"deploy"|"delete",
  "target_type": "code"|"tests"|"config"|"service"|"docs"|"data"|"infrastructure",
  "framework": e.g. "playwright"|"pytest"|"cargo"|"jest"|"docker" or "" if generic,
  "scope": "single"|"multi_file"|"cross_service"|"system_wide",
  "confidence": 0.0..1.0
}}
}}

Intent meaning:
- chat=conversational, no action; debug=find root cause of failure; fix=repair specific known bug;
- refactor=restructure no behavior change; test=WRITE new tests; docs=write documentation;
- code_read=read/inspect files; architecture=high-level design; file_ops=create/delete files;
- system_admin=configure services/deploy.

CRITICAL:
- "scrivi test per X" → intent=test, action_verb=write.
- "esegui test e correggi fail" / "fai funzionare i test" → intent=debug, action_verb=resolve.
- "fix bug at file.py:42" → intent=fix, action_verb=resolve, scope=single.
- "leggi file.py" → intent=code_read, action_verb=read.

Use confidence<0.7 honestly when ambiguous (downstream asks user). NEVER inflate.

Examples:
- "ciao" → {{"intent":"chat","agentic_score":0.0,"requires_tools":false,"complexity":"low","confidence":0.99,"candidates":[{{"intent":"chat","confidence":0.99}}],"slots":{{"action_verb":"read","target_type":"code","framework":"","scope":"single","confidence":0.10}}}}
- "leggi src/main.py" → {{"intent":"code_read","agentic_score":0.8,"requires_tools":true,"complexity":"low","confidence":0.95,"candidates":[{{"intent":"code_read","confidence":0.95}}],"slots":{{"action_verb":"read","target_type":"code","framework":"","scope":"single","confidence":0.95}}}}
- "scrivi un test per foo()" → {{"intent":"test","agentic_score":0.7,"requires_tools":true,"complexity":"medium","confidence":0.92,"candidates":[{{"intent":"test","confidence":0.92}}],"slots":{{"action_verb":"write","target_type":"tests","framework":"","scope":"single","confidence":0.90}}}}
- "esegui i test playwright e risolvi i fail" → {{"intent":"debug","agentic_score":0.95,"requires_tools":true,"complexity":"high","confidence":0.85,"candidates":[{{"intent":"debug","confidence":0.85}},{{"intent":"fix","confidence":0.70}}],"slots":{{"action_verb":"resolve","target_type":"tests","framework":"playwright","scope":"multi_file","confidence":0.92}}}}
- "i test pytest non passano, correggi" → {{"intent":"debug","agentic_score":0.9,"requires_tools":true,"complexity":"high","confidence":0.80,"candidates":[{{"intent":"debug","confidence":0.80}},{{"intent":"fix","confidence":0.65}}],"slots":{{"action_verb":"resolve","target_type":"tests","framework":"pytest","scope":"multi_file","confidence":0.88}}}}
- "fix null pointer at handlers.py:42" → {{"intent":"fix","agentic_score":0.85,"requires_tools":true,"complexity":"medium","confidence":0.90,"candidates":[{{"intent":"fix","confidence":0.90}}],"slots":{{"action_verb":"resolve","target_type":"code","framework":"","scope":"single","confidence":0.85}}}}
- "deploya il microservizio doc-service" → {{"intent":"system_admin","agentic_score":0.9,"requires_tools":true,"complexity":"high","confidence":0.92,"candidates":[{{"intent":"system_admin","confidence":0.92}}],"slots":{{"action_verb":"deploy","target_type":"service","framework":"docker","scope":"cross_service","confidence":0.90}}}}
- "elimina i dockerfile rimasti" → {{"intent":"file_ops","agentic_score":0.7,"requires_tools":true,"complexity":"low","confidence":0.88,"candidates":[{{"intent":"file_ops","confidence":0.88}}],"slots":{{"action_verb":"delete","target_type":"infrastructure","framework":"docker","scope":"multi_file","confidence":0.85}}}}

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
        # Soglie disambiguazione (mig 0132) — usate da _validate_parsed per
        # decidere se il classifier deve chiedere chiarimenti all'utente.
        try:
            self._ambiguity_min_confidence = float(
                cfg["routing.ambiguity_min_confidence"]
            )
        except (ValueError, KeyError):
            self._ambiguity_min_confidence = DEFAULT_AMBIGUITY_MIN_CONFIDENCE
        try:
            self._ambiguity_min_margin = float(cfg["routing.ambiguity_min_margin"])
        except (ValueError, KeyError):
            self._ambiguity_min_margin = DEFAULT_AMBIGUITY_MIN_MARGIN

    @staticmethod
    def _cache_key(message: str) -> str:
        head = message.strip()[:1000]
        return hashlib.sha256(head.encode("utf-8")).hexdigest()

    @staticmethod
    def _parse_slots(parsed: dict) -> ActionSlots:
        """Estrae e valida i 4 slot canonici dal JSON LLM (Livello 4 NLU).

        Ritorna `ActionSlots()` vuoto se il campo `slots` e' assente o
        malformato — in quel caso il caller fa fallback al routing classico
        (intent, behavior_mode). Niente eccezioni: l'estrazione e' best-effort.
        """
        raw = parsed.get("slots")
        if not isinstance(raw, dict):
            return ActionSlots()
        try:
            action_verb = str(raw.get("action_verb", "")).strip().lower()
            target_type = str(raw.get("target_type", "")).strip().lower()
            framework = str(raw.get("framework", "")).strip().lower()
            scope = str(raw.get("scope", "")).strip().lower()
            conf_raw = raw.get("confidence", 0.0)
            confidence = max(0.0, min(1.0, float(conf_raw)))
        except (ValueError, TypeError):
            return ActionSlots()
        # Validazione enum: valori non canonici → svuota i campi corrispondenti
        # (non solleviamo, il caller fa fallback). Framework e' free-form.
        if action_verb not in ALLOWED_ACTION_VERBS:
            action_verb = ""
        if target_type not in ALLOWED_TARGET_TYPES:
            target_type = ""
        if scope not in ALLOWED_SCOPES:
            scope = ""
        return ActionSlots(
            action_verb=action_verb,
            target_type=target_type,
            framework=framework,
            scope=scope,
            confidence=confidence,
        )

    @staticmethod
    def _parse_candidates(parsed: dict, top_intent: str, top_conf: float) -> list[IntentCandidate]:
        """Estrae fino a 3 candidati validati dal campo `candidates` del JSON.
        Se assente o malformato, ritorna [top_intent]."""
        raw = parsed.get("candidates")
        if not isinstance(raw, list):
            return [IntentCandidate(intent=top_intent, confidence=top_conf)]
        out: list[IntentCandidate] = []
        for item in raw[:3]:  # max 3
            if not isinstance(item, dict):
                continue
            try:
                cand_intent = str(item["intent"]).strip().lower()
                if cand_intent not in ALLOWED_INTENTS:
                    continue
                cand_conf = max(0.0, min(1.0, float(item["confidence"])))
                out.append(IntentCandidate(intent=cand_intent, confidence=cand_conf))
            except (KeyError, ValueError, TypeError):
                continue
        if not out:
            return [IntentCandidate(intent=top_intent, confidence=top_conf)]
        # Garantisce ordinamento DESC per confidence
        out.sort(key=lambda c: c.confidence, reverse=True)
        return out

    @staticmethod
    def _is_ambiguous(
        candidates: list[IntentCandidate],
        min_confidence: float = DEFAULT_AMBIGUITY_MIN_CONFIDENCE,
        min_margin: float = DEFAULT_AMBIGUITY_MIN_MARGIN,
    ) -> bool:
        """True se il classificatore non e' abbastanza sicuro per agire.

        Best practice NLU (Rasa/Dialogflow): se la top confidence e' bassa
        OPPURE il margine sul secondo candidato e' stretto, chiediamo
        all'utente di disambiguare invece di indovinare.

        Le soglie sono DB-driven (settings.routing.ambiguity_min_*, mig 0132)
        e vengono passate dal caller (`_validate_parsed`). I default tecnici
        sono usati SOLO se il DB non e' disponibile (fallback).
        """
        if not candidates:
            return True
        top = candidates[0]
        if top.confidence < min_confidence:
            return True
        if len(candidates) >= 2:
            margin = top.confidence - candidates[1].confidence
            if margin < min_margin:
                return True
        return False

    @staticmethod
    def _validate_parsed(
        parsed: dict,
        ambiguity_min_confidence: float = DEFAULT_AMBIGUITY_MIN_CONFIDENCE,
        ambiguity_min_margin: float = DEFAULT_AMBIGUITY_MIN_MARGIN,
    ) -> Optional[AgenticIntent]:
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
            # Parsing candidati + flag ambiguity (best practice NLU).
            # Le soglie sono DB-driven (mig 0132) passate dal caller.
            candidates = AgenticIntentClassifier._parse_candidates(
                parsed, intent, confidence,
            )
            is_ambiguous = AgenticIntentClassifier._is_ambiguous(
                candidates,
                min_confidence=ambiguity_min_confidence,
                min_margin=ambiguity_min_margin,
            )
            # Slot filling (Livello 4 NLU): se il campo manca o e' invalido,
            # ritorna ActionSlots() vuoto — il caller usera' routing classico.
            slots = AgenticIntentClassifier._parse_slots(parsed)
            return AgenticIntent(
                intent=intent,
                agentic_score=agentic_score,
                requires_tools=requires_tools,
                complexity=complexity,
                confidence=confidence,
                model_used="",  # riempito dopo
                candidates=candidates,
                is_ambiguous=is_ambiguous,
                slots=slots,
            )
        except (KeyError, ValueError, TypeError) as exc:
            logger.warning("classifier: parsed JSON malformed: %s", exc)
            return None

    @staticmethod
    def _extract_json(content: str) -> Optional[dict]:
        """Estrae il primo oggetto JSON dal contenuto LLM (anche se circondato
        da markdown fences, testo, o annidato N livelli).

        Robustezza:
        1. Strip code fences ``` ```json
        2. Tentativo parse diretto del contenuto pulito
        3. Brace-matching counter: trova il primo `{` e cerca la `}` bilanciata
           a qualsiasi profondita' di annidamento (slots e' nested level 2)
        4. Se ancora fallisce, fallback al regex single-level (legacy)
        """
        if not content:
            return None
        # Strip markdown code fences
        content = re.sub(r"^```(?:json)?\s*", "", content.strip())
        content = re.sub(r"\s*```\s*$", "", content)
        # 1. Parse diretto
        try:
            return json.loads(content)
        except json.JSONDecodeError:
            pass
        # 2. Brace-matching counter (gestisce N livelli annidati)
        start = content.find("{")
        if start >= 0:
            depth = 0
            in_string = False
            escape = False
            for i in range(start, len(content)):
                ch = content[i]
                if escape:
                    escape = False
                    continue
                if ch == "\\":
                    escape = True
                    continue
                if ch == '"':
                    in_string = not in_string
                    continue
                if in_string:
                    continue
                if ch == "{":
                    depth += 1
                elif ch == "}":
                    depth -= 1
                    if depth == 0:
                        # Trovata graffa di chiusura bilanciata
                        candidate = content[start : i + 1]
                        try:
                            return json.loads(candidate)
                        except json.JSONDecodeError:
                            break  # cade nel fallback regex
        # 3. Fallback legacy regex (single-level nesting)
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
        adjusted_conf = confidence * 0.5  # ridotta perche' fallback
        # Fallback path: nessuna lista di candidati alternativi disponibile.
        # is_ambiguous=True spinge il caller a chiedere disambiguazione,
        # coerente con il fatto che la confidence ridotta indica incertezza.
        # Soglia DB-driven (mig 0132) — _ensure_config() la popola su istanza.
        min_conf = getattr(
            self, "_ambiguity_min_confidence", DEFAULT_AMBIGUITY_MIN_CONFIDENCE
        )
        candidates = [IntentCandidate(intent=intent, confidence=adjusted_conf)]
        return AgenticIntent(
            intent=intent,
            agentic_score=agentic_score,
            requires_tools=intent != "chat",
            complexity="medium",
            confidence=adjusted_conf,
            model_used=f"fallback:{reason}",
            cached=False,
            fallback_used=True,
            candidates=candidates,
            is_ambiguous=adjusted_conf < min_conf,
        )

    async def classify(self, message: str) -> AgenticIntent:
        """Punto di ingresso: cache-first → LLM → fallback keyword.
        Carica config da DB (mig 0111) al primo call con cache 60s."""
        if not message or not message.strip():
            return self._fallback_result(message, "empty_message")

        # Lazy load config (provider/model/cache size/timeout) da settings DB.
        # Se la config non e' disponibile (DB down o settings mancanti), degrada
        # al classifier keyword invece di usare un modello hardcoded a caso.
        try:
            await self._ensure_config()
        except ClassifierConfigUnavailable as exc:
            logger.warning(
                "classifier: config DB non disponibile (%s), uso fallback keyword",
                exc,
            )
            return self._fallback_result(message, "config_unavailable")
        timeout_s = getattr(self, "_llm_timeout", DEFAULT_LLM_TIMEOUT_SECONDS)

        key = self._cache_key(message)
        cached = await self._cache.get(key)
        if cached is not None:
            result = AgenticIntent(**cached)
            result.cached = True
            return result

        # Chiamata LLM con timeout configurabile.
        # Pattern chain (mig 0134): itera nexus_classifier_provider_chain
        # finche' un provider risponde con JSON valido. Se la chain e' vuota
        # cade sul singolo (self._provider, self._model) per retrocompat.
        prompt = _CLASSIFIER_PROMPT.format(message=message[:2000])
        chain_db = await _load_classifier_chain()
        if chain_db:
            chain: list[tuple[str, str]] = [(e.provider, e.model) for e in chain_db]
        else:
            # Fallback retrocompat: chain a 1 elemento dalle settings (mig 0132)
            chain = [(self._provider, self._model)]

        ambiguity_args = dict(
            ambiguity_min_confidence=getattr(
                self, "_ambiguity_min_confidence", DEFAULT_AMBIGUITY_MIN_CONFIDENCE
            ),
            ambiguity_min_margin=getattr(
                self, "_ambiguity_min_margin", DEFAULT_AMBIGUITY_MIN_MARGIN
            ),
        )

        attempted: list[str] = []
        last_failure_reason: str = "all_providers_failed"
        for (cl_provider, cl_model) in chain:
            attempted.append(f"{cl_provider}/{cl_model}")
            try:
                llm_call = self._providers.generate_completion_async(
                    cl_provider, cl_model, prompt
                )
                llm_result = await asyncio.wait_for(llm_call, timeout=timeout_s)
            except asyncio.TimeoutError:
                logger.warning(
                    "classifier chain[%s/%s]: timeout (%ss), provo prossimo",
                    cl_provider, cl_model, timeout_s
                )
                last_failure_reason = "timeout"
                continue
            except Exception as exc:  # noqa: BLE001
                logger.warning(
                    "classifier chain[%s/%s]: exception (%s), provo prossimo",
                    cl_provider, cl_model, type(exc).__name__
                )
                last_failure_reason = f"llm_error:{type(exc).__name__}"
                continue

            content = getattr(llm_result, "content", "") or ""
            # Detection esplicito di provider error inline (es. Gemini
            # ritorna "[Error: This model is currently experiencing high demand...]")
            # NON e' un'exception ma il content e' inutile.
            stripped = content.strip()
            if stripped.startswith("[Error:") or stripped.startswith("[error:"):
                logger.warning(
                    "classifier chain[%s/%s]: provider returned error inline (%s...), provo prossimo",
                    cl_provider, cl_model, stripped[:60]
                )
                last_failure_reason = "provider_inline_error"
                continue

            parsed = self._extract_json(content)
            if parsed is None:
                logger.warning(
                    "classifier chain[%s/%s]: cannot parse JSON, provo prossimo",
                    cl_provider, cl_model
                )
                last_failure_reason = "json_parse"
                continue

            validated = self._validate_parsed(parsed, **ambiguity_args)
            if validated is None:
                logger.warning(
                    "classifier chain[%s/%s]: JSON valido ma schema invalido, provo prossimo",
                    cl_provider, cl_model
                )
                last_failure_reason = "json_validation"
                continue

            # SUCCESS: il provider ha risposto con JSON valido.
            validated.model_used = getattr(llm_result, "model", cl_model)
            # Telemetria: chain attempts utile per audit/dashboard.
            if len(attempted) > 1:
                logger.info(
                    "classifier chain: vinto al tentativo %d/%d (%s/%s); attempts=%s",
                    len(attempted), len(chain), cl_provider, cl_model,
                    ",".join(attempted),
                )
            await self._cache.put(key, asdict(validated))
            return validated

        # Tutti i provider della chain hanno fallito.
        logger.error(
            "classifier chain ESAURITA dopo %d tentativi (%s); fallback keyword. Ultimo motivo: %s",
            len(attempted), ",".join(attempted), last_failure_reason,
        )
        return self._fallback_result(message, f"chain_exhausted:{last_failure_reason}")

    async def stats(self) -> dict:
        """Esposto per debug/monitoring."""
        cache_stats = await self._cache.stats()
        return {
            "cache": cache_stats,
            "provider": self._provider,
            "model": self._model,
        }
