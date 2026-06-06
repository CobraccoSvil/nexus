"""Semantic router with embedding-based intent classification."""
from __future__ import annotations

import logging
from dataclasses import dataclass
import urllib.parse

import numpy as np

logger = logging.getLogger(__name__)

# Intent exemplars for embedding-based classification
_INTENT_EXEMPLARS: dict[str, list[str]] = {
    "fix": [
        "fix this bug", "debug the error", "why is it crashing",
        "resolve the issue", "patch the vulnerability", "hotfix needed",
        # Pannello Ottimizzazione Nexus: "Fix questo problema nel file X: ..."
        "fix questo problema nel file", "fix this problem in the file",
        "risolvi questo problema nel file", "correggi questo problema nel file",
        "fix the issue in the file", "fix the problem in the file",
        # Osservazioni di malfunzionamento su elementi gia' presenti nel progetto
        # (segnalazione di un problema concreto, non conversazione generica).
        "il menu non funziona", "i link del menu sono sbagliati",
        "le form sono mal disposte", "i campi del form sono troppo piccoli",
        "il layout della pagina non e' rispettato", "la pagina e' rotta",
    ],
    "refactor": [
        "refactor this code", "clean up the implementation", "improve code quality",
        "simplify the logic", "extract a function", "reduce complexity",
        # Pannello Ottimizzazione Nexus: "Long function X — N lines (threshold: 50)"
        "long function", "funzione troppo lunga", "lines threshold",
        "the function is too long", "function exceeds threshold",
        "spezza questa funzione", "estrai funzioni helper", "extract helper functions",
        "refactorizza questa funzione troppo lunga", "reduce function length",
    ],
    "test": [
        "write tests", "add unit tests", "create test cases",
        "test coverage", "write integration tests", "add assertions",
    ],
    "docs": [
        "write documentation", "add comments", "document the API",
        "update the README", "add JSDoc",
        "genera documento", "genera documentazione", "genera analisi",
        "analisi tecnica", "analisi funzionale", "genera report",
        "release notes", "diagramma ER", "gestione progetto",
        "genera l'analisi", "documenta il progetto", "scrivi documentazione",
        "crea documento", "genera il documento", "document generation",
    ],
    # ── code_read: lettura/ispezione di file e codice sorgente ──────────────
    # Distingue le query di LETTURA (vuole vedere il contenuto) da quelle di
    # DOCUMENTAZIONE (vuole produrre testo descrittivo). Senza questo intent
    # separato, "leggi il file X" e "elenca i file" cadevano su `docs` e il
    # RAG inline (BP7) non si attivava mai.
    "code_read": [
        "read the file", "show me the file", "view the file content",
        "list files in the directory", "what files are in", "ls the folder",
        "cat the file", "how many lines", "quante righe ha il file",
        "leggi il file", "mostra il file", "mostra il contenuto del file",
        "elenca i file", "elenca le directory", "lista dei file",
        "cosa contiene il file", "cosa c'è nel file", "mostrami il codice",
        "show me the code", "view this file", "read this code",
        "mostra il codice di", "leggi il codice", "apri il file",
        "conta le righe", "quante funzioni", "cosa fa questa classe",
        "elenco dei file", "struttura del progetto", "tree della cartella",
        # Domande conoscitive sull'esistenza/scopo di entita' del progetto
        # attivo: richiedono ispezione dei file reali, non risposta in astratto.
        "perche ci sono due file index.html", "perche esiste questo file",
        "a cosa serve questo file nel progetto",
        "come mai ci sono due cartelle uguali",
        "che cosa rappresenta questa funzione",
    ],
    # ── architecture: design di sistema E scaffolding applicativo ────────────
    # Oltre al design "puro" (architettura, migrazione, valutazione approcci),
    # questo intent copre lo SCAFFOLDING applicativo: "crea un'applicazione",
    # "fai una app per X", "implementa l'app dal file". Senza questi exemplar
    # la frase "crea l'app descritta nel file allegato" cadeva su `code_read`
    # (vicinanza semantica al token "file") -> profilo di sola lettura -> il
    # modello esplorava l'allegato invece di scrivere i file.
    "architecture": [
        "design the system", "create the architecture", "plan the migration",
        "evaluate approaches", "review the design", "system design",
        # Scaffolding applicativo (italiano) — famiglia verbo+oggetto
        "crea un'applicazione", "crea l'applicazione", "fai una app per",
        "costruisci un sistema gestionale", "sviluppa un sito web",
        "realizza una piattaforma", "implementa l'applicazione descritta nel file",
        "crea l'app dal mockup figma", "genera un progetto fullstack",
        "crea un gestionale", "crea un e-commerce", "crea una dashboard",
        # Scaffolding applicativo (inglese)
        "scaffold a fullstack application", "build a web app for",
        "create an application for", "develop a booking system",
    ],
    "chat": [
        "hello", "how are you", "help me", "what is", "explain",
        "tell me about", "general question",
    ],
    "database_schema_change": [
        "aggiungi una colonna", "crea tabella", "modifica schema",
        "rinomina colonna", "elimina tabella", "aggiungi indice",
        "alter table", "create table", "add column", "drop column",
        "drop table", "rename column", "modify schema", "schema migration",
        "create migration", "database migration", "migrazione database",
        "aggiorna schema", "rimuovi colonna", "foreign key", "chiave esterna",
    ],
    # Operazioni su filesystem: creare/eliminare/spostare/rinominare file e directory.
    # Richiede modelli con tool use solido — i modelli "lite" tendono a interpretare
    # liberamente (es. "elimina file Docker" diventa "ricrea i file Docker").
    "file_ops": [
        "elimina questi file", "rimuovi i file", "cancella la cartella",
        "delete these files", "remove the files", "rm the files",
        "sposta il file", "move the file", "rename file",
        "ripulisci la cartella", "cleanup the directory",
        "elimina configurazione", "remove configuration",
        "elimina dockerfile", "rimuovi dockerfile", "elimina docker-compose",
    ],
    # Amministrazione sistema/runtime: docker, systemd, processi, servizi, container.
    # Necessita di modelli capaci di reasoning sui side-effect (un comando sbagliato
    # puo' fermare l'infrastruttura, vedi safety_progetto nei system prompt).
    "system_admin": [
        "ferma il container", "avvia il container", "stop container",
        "restart container", "docker compose down", "docker compose up",
        "elimina container", "remove container", "delete container",
        "ferma il servizio", "start service", "systemctl restart",
        "stop service", "kill process", "termina processo",
        "ripulisci docker", "docker prune", "docker system prune",
        "elimina docker", "rimuovi docker", "rimuovi tutta la configurazione docker",
    ],
}

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

    def __init__(self, embedding_service: object | None = None) -> None:
        self._embedding_service = embedding_service
        self._intent_vectors: dict[str, list[list[float]]] | None = None
        self._use_embeddings = False

    def _init_intent_vectors(self) -> None:
        """Pre-compute intent exemplar embeddings."""
        if self._embedding_service is None or self._intent_vectors is not None:
            return
        try:
            svc = self._embedding_service
            self._intent_vectors = {}
            for intent, exemplars in _INTENT_EXEMPLARS.items():
                vectors = svc.embed_batch("", exemplars)
                self._intent_vectors[intent] = [v.values for v in vectors]
            self._use_embeddings = True
            logger.info("Semantic router initialized with embedding-based classification")
        except Exception as e:
            logger.warning("Failed to init intent vectors, falling back to keywords: %s", e)
            self._use_embeddings = False

    def classify_intent(self, message: str) -> dict[str, str]:
        """Classify user intent using embeddings or keyword fallback."""
        self._init_intent_vectors()

        if self._use_embeddings and self._embedding_service is not None:
            return self._classify_by_embedding(message)
        return self._classify_by_keywords(message)

    def _classify_by_embedding(self, message: str) -> dict[str, str]:
        svc = self._embedding_service
        query_vec = np.array(svc.embed_text("", message).values)

        best_intent = "chat"
        best_score = -1.0

        for intent, vectors in (self._intent_vectors or {}).items():
            similarities = [
                float(np.dot(query_vec, np.array(v)) / (np.linalg.norm(query_vec) * np.linalg.norm(v) + 1e-8))
                for v in vectors
            ]
            avg_sim = sum(sorted(similarities, reverse=True)[:3]) / min(3, len(similarities))
            if avg_sim > best_score:
                best_score = avg_sim
                best_intent = intent

        return {"intent": best_intent, "confidence": f"{best_score:.2f}"}

    @staticmethod
    def _classify_by_keywords(message: str) -> dict[str, str]:
        lowered = message.lower()
        intent_keywords = {
            # fix/refactor valutati PRIMA di code_read: il pannello Ottimizzazione
            # invia messaggi tipo "Fix questo problema nel file ...: Long function"
            # che senza questa priorita' cadrebbero su code_read (vede "nel file").
            "fix": [
                "/fix", "bug", "error", "crash", "broken", "debug", "issue", "patch",
                "fix questo problema", "fix this problem", "risolvi questo problema",
                "risolvi il problema", "correggi questo", "correggi il",
                "fix the issue", "fix the bug",
                # Osservazioni di malfunzionamento/UI su qualcosa che esiste GIA'
                # nel progetto: l'utente segnala un problema concreto, non fa
                # small-talk. Senza questi pattern "il menu non funziona" o "le
                # form sono mal disposte" cadevano su chat -> risposta generica
                # su progetti ipotetici (audit chat progetto Marco, 06/06/2026).
                "non funziona", "non funzionano", "non va", "non vanno",
                "e' rotto", "è rotto", "sono rotti", "sono rotte", "si rompe",
                "mal disposto", "mal disposti", "mal disposta", "mal disposte",
                "non rispetta il layout", "non rispettano il layout",
                "non rispetta lo stile", "fuori posto", "sballato", "sballati",
                "è sbagliato", "e' sbagliato", "sono sbagliati", "sono sbagliate",
                "link sbagliati", "link errati", "percorsi sbagliati",
                "campi piccoli", "campi sono piccoli", "troppo piccoli",
                "non si vede", "non si vedono", "visualizzazione sbagliata",
            ],
            "refactor": [
                "/refactor", "refactor", "clean", "simplify", "extract", "improve",
                "long function", "funzione troppo lunga", "troppe righe",
                "threshold:", "lines (threshold", "righe (soglia",
                "riduci la funzione", "spezza la funzione", "split function",
                "troppo lunga", "eccessivamente lunga",
            ],
            # code_read va valutato DOPO fix/refactor per evitare che "leggi il
            # file" / "elenca i file" cadano su docs (solo perché "explain" e
            # "mostra" matchano gli exemplar della documentazione). Con fix/refactor
            # prima, i messaggi del pannello Ottimizzazione vengono instradati correttamente.
            "code_read": [
                "leggi il file", "leggi file", "leggi il codice",
                "mostra il file", "mostrami il file", "mostrami il codice",
                "elenca i file", "elenca file", "lista file", "lista dei file",
                "elenco file", "elenco dei file", "struttura del progetto",
                # Domande "quanti/quante X" su entita' del codice. Catturare
                # generico: "quante variabili", "quanti file", "quanti componenti",
                # "quante tabelle" + i casi specifici storici. Evita che cadano
                # erroneamente su 'docs' (visto bug audit 27/05/2026 dove
                # "quante variabili ci sono nel progetto" attivava il planner +
                # verifier di docs con loop infinito).
                "quante righe", "quante funzioni", "quante classi",
                "quante variabili", "quanti file", "quanti componenti",
                "quanti moduli", "quante tabelle", "quanti test", "quanti errori",
                "quante linee", "quanti record", "quante chiamate",
                "quanti endpoint", "quanti import", "quanti package",
                "cosa contiene il file", "cosa c'è nel file",
                "read the file", "read file", "show the file", "view file",
                "list files", "list the files", "cat ", "head ", "tail ",
                "how many lines", "how many functions", "how many files",
                "how many classes", "how many variables", "how many tests",
                "cosa fa questa classe", "cosa fa questa funzione",
                "tree della cartella", "mostra il contenuto",
                # Domande generali sul progetto/codebase, intent informativo.
                "ci sono nel progetto", "esistono nel progetto",
                "dove si trova", "dove sta", "in quale file",
                # Domande conoscitive sull'ESISTENZA o lo SCOPO di entita' del
                # progetto attivo: vanno ispezionate sui file reali, non risposte
                # in astratto. Includiamo le varianti senza accento ("perche")
                # perche' gli utenti spesso le digitano cosi' (caso reale:
                # "perche ci sono due file index.html?" -> prima cadeva su chat).
                "perché c'è", "perché ci sono", "perche c'è", "perche ci sono",
                "perché esiste", "perché esistono", "perche esiste", "perche esistono",
                "perché ho due", "perché ci sono due", "perche ci sono due",
                "perché abbiamo due", "come mai c'è", "come mai ci sono",
                "come mai esiste", "a cosa serve", "a che serve",
                "cosa rappresenta", "che cos'è questo file", "che file è",
            ],
            # Pattern test piu' specifici: il bare "test" matchava anche
            # nomi file come "test.js" o "test.spec.ts" facendo cadere
            # "delete the file test.js" su intent=test invece di file_ops.
            # Ora richiede contesto di azione/sostantivo composto.
            "test": [
                "/test", "esegui test", "esegui i test", "lancia test", "lancia i test",
                "esegui i test unitari", "run tests", "run the tests",
                "scrivi test", "scrivi un test", "scrivi i test", "write tests",
                "write a test", "write the test", "aggiungi test", "add tests",
                "coverage", "code coverage", "test coverage",
                "assert ", "assertion", "expect(",
                "test unitari", "test unitario", "unit test", "unit tests",
                "test di integrazione", "integration test",
                "test e2e", "end-to-end test", "playwright test", "spec file",
            ],
            "docs": ["/docs", "document", "readme", "jsdoc", "comment",
                     "genera documento", "genera analisi", "analisi tecnica",
                     "analisi funzionale", "genera report", "release notes",
                     "diagramma er", "gestione progetto", "genera l'analisi",
                     "documenta", "documentazione", "genera il documento"],
            "architecture": ["/arch", "architecture", "design", "system", "migrate", "plan"],
            "database_schema_change": [
                "crea tabella", "create table", "alter table", "add column",
                "aggiungi colonna", "drop table", "drop column", "modifica schema",
                "schema migration", "migrazione", "foreign key", "create index",
            ],
            # File operations: detect verbi azionali su file/cartelle del progetto.
            # Order matters: file_ops va valutato prima di system_admin per evitare
            # collisioni quando il task riguarda file di config Docker (es.
            # "elimina i file Dockerfile" -> file_ops, non system_admin).
            "file_ops": [
                "elimina file", "rimuovi file", "cancella file", "remove file",
                "delete file", "elimina i file", "remove the file",
                # Pattern con articolo davanti al sostantivo (errore audit 28/05/2026:
                # "cancella il file variables.txt" cadeva su code_read invece di file_ops
                # perche' i pattern qui sotto non includevano l'articolo)
                "elimina il file", "rimuovi il file", "cancella il file",
                "delete the file", "remove this file", "delete this file",
                "elimina questo file", "cancella questo file", "rimuovi questo file",
                "rinomina il file", "rinomina file", "rename file", "rename the file",
                "sposta il file", "sposta file", "move file", "move the file",
                "crea il file", "crea un file", "create file", "create the file",
                "elimina la cartella", "rimuovi la cartella", "delete folder",
                "elimina dockerfile", "rimuovi dockerfile",
                "elimina docker-compose", "rimuovi docker-compose",
                "elimina configurazione docker", "remove docker configuration",
                "elimina file di configurazione", "rimuovi file di configurazione",
                "ripulisci la directory", "cleanup directory",
            ],
            # System administration: docker runtime, systemd, processi, container.
            "system_admin": [
                "docker stop", "docker rm", "docker prune", "system prune",
                "ferma il container", "stop container", "kill container",
                "elimina container", "remove container", "delete container",
                "ferma il servizio", "stop service", "systemctl stop",
                "systemctl restart", "restart service",
                "compose down", "compose up", "docker compose",
                "elimina docker", "rimuovi docker locale", "elimina docker locale",
            ],
        }

        for intent, keywords in intent_keywords.items():
            matches = sum(1 for kw in keywords if kw in lowered)
            if matches >= 1:
                confidence = min(0.95, 0.75 + matches * 0.05)
                return {"intent": intent, "confidence": f"{confidence:.2f}"}

        return {"intent": "chat", "confidence": "0.82"}

    # NOTA Fase A consolidamento (vedi piano `questo-lo-stesso-proud-blossom.md`):
    #
    # La matrice di routing _ROUTING_MATRIX e' stata RIMOSSA da questo file.
    # La fonte autoritativa e' ora `crates/mcp-core/src/orchestrator.rs`
    # (funzioni `classify_intent_local`, `is_risky_task`, `route_model_with_mode`,
    #  `route_model_from_catalog`), esposte via endpoint REST
    # `POST /api/internal/routing/decide`.
    #
    # `route_model()` di questo router e' diventato un thin client che chiama
    # quell'endpoint con timeout breve e cache 30s. Cosi' il brain non duplica
    # piu' la matrice e non serve sincronizzare manualmente le 50+ entry tra
    # Python e Rust.
    #
    # Cio' che resta in Python:
    #   - `classify_intent` (embedding-based + keyword fallback): la classificazione
    #     semantica e' utile per altri usi (analytics, profiling) ed e' la parte
    #     che Rust non sa fare bene (no embedding service nativo Rust).
    #   - L'endpoint `/route-model` per backward-compat: ora delega a Rust.

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
