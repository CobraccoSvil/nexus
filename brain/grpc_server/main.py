"""Neural Core entry point.

Runs the gRPC server by default.
Pass --rest to also start the FastAPI debug server on port 8001.
"""
from __future__ import annotations

import asyncio
import base64
import hashlib
import hmac
import json as json_mod
import logging
import os
import select
import struct
import sys
import threading
import time
import tempfile
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

try:
    import fcntl
    import pty
    import termios

    POSIX_PTY = True
except ImportError:
    fcntl = None
    pty = None
    termios = None
    POSIX_PTY = False

from fastapi import FastAPI, WebSocket, WebSocketDisconnect
from fastapi.responses import StreamingResponse
from pydantic import BaseModel

from starlette.middleware.cors import CORSMiddleware

from brain.embeddings import EmbeddingService
from brain.providers import ProviderRegistry
from brain.router import SemanticRouter
from brain.router.agentic_classifier import AgenticIntentClassifier

logger = logging.getLogger(__name__)
_SETTINGS_CACHE: dict[str, tuple[float, str]] = {}
_SETTINGS_CACHE_TTL_SECONDS = 15.0


def _mcp_core_url() -> str:
    return (os.environ.get("MCP_CORE_URL") or "http://127.0.0.1:4000").rstrip("/")


def _get_core_setting(key: str) -> str:
    now = time.time()
    cached = _SETTINGS_CACHE.get(key)
    if cached and now - cached[0] <= _SETTINGS_CACHE_TTL_SECONDS:
        return cached[1]

    value = ""
    url = f"{_mcp_core_url()}/internal/settings/{urllib.parse.quote(key)}"
    try:
        with urllib.request.urlopen(url, timeout=2.0) as response:
            payload = json_mod.loads(response.read().decode("utf-8"))
            if isinstance(payload, dict):
                raw = payload.get("value")
                if isinstance(raw, str):
                    value = raw.strip()
    except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError, json_mod.JSONDecodeError):
        value = cached[1] if cached else ""

    _SETTINGS_CACHE[key] = (now, value)
    return value


def _terminal_secret() -> str:
    from_env = (os.environ.get("TERMINAL_SESSION_SECRET") or "").strip()
    if from_env:
        return from_env

    from_db = _get_core_setting("terminal_session_secret")
    if from_db:
        return from_db

    jwt_secret = _get_core_setting("jwt_secret")
    if jwt_secret:
        return jwt_secret

    return "development-terminal-secret-change-me"


def _allowed_roots() -> list[Path]:
    raw = os.environ.get("PROJECTS_ALLOWED_ROOTS", "")
    candidates = [item.strip() for item in raw.split(os.pathsep) if item.strip()]
    configured_base_root = _get_core_setting("projects_base_root")
    if configured_base_root:
        candidates.append(configured_base_root)
    if not candidates:
        candidates = [os.getcwd()]

    roots: list[Path] = []
    seen: set[str] = set()
    for item in candidates:
        try:
            candidate = Path(item).expanduser().resolve()
        except Exception:
            continue
        if not candidate.exists() or not candidate.is_dir():
            continue
        key = str(candidate)
        if key in seen:
            continue
        seen.add(key)
        roots.append(candidate)

    if not roots:
        roots = [Path(os.getcwd()).expanduser().resolve()]

    return roots


def _path_within(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def _decode_token_segment(segment: str) -> bytes:
    padding = "=" * (-len(segment) % 4)
    return base64.urlsafe_b64decode(segment + padding)


def _verify_terminal_token(token: str | None) -> dict[str, object] | None:
    if not token or "." not in token:
        return None

    payload_segment, signature = token.split(".", 1)
    expected = hashlib.sha256(f"{_terminal_secret()}:{payload_segment}".encode()).hexdigest()
    if not hmac.compare_digest(signature, expected):
        return None

    try:
        payload = json_mod.loads(_decode_token_segment(payload_segment).decode())
    except Exception:
        return None

    exp = int(payload.get("exp", 0))
    if exp <= int(time.time()):
        return None

    cwd = payload.get("cwd")
    if not isinstance(cwd, str) or not cwd:
        return None

    root = payload.get("root")
    if not isinstance(root, str) or not root:
        return None

    resolved_cwd = Path(cwd).expanduser().resolve()
    resolved_root = Path(root).expanduser().resolve()

    if not any(_path_within(resolved_root, allowed_root) for allowed_root in _allowed_roots()):
        return None

    if not _path_within(resolved_cwd, resolved_root):
        return None

    payload["cwd"] = str(resolved_cwd)
    payload["root"] = str(resolved_root)
    return payload


def _default_shell() -> list[str]:
    if os.name == "nt":
        return [os.environ.get("TERMINAL_SHELL", "powershell.exe")]
    shell = os.environ.get("TERMINAL_SHELL", "bash")
    if shell.endswith("bash") or shell == "bash":
        return [shell, "--login"]
    return [shell]


def _prepare_shell_command(payload: dict[str, object]) -> tuple[list[str], str | None]:
    command = _default_shell()
    root_raw = payload.get("root")
    if not isinstance(root_raw, str) or not root_raw:
        return command, None
    cwd_raw = payload.get("cwd")
    initial_cwd = str(Path(cwd_raw).expanduser().resolve()) if isinstance(cwd_raw, str) and cwd_raw else str(Path(root_raw).expanduser().resolve())

    root = Path(root_raw)
    shell_exe = Path(command[0]).name.lower() if command else ""
    is_bash = "bash" in shell_exe
    if not is_bash:
        return command, None

    # Guard bash: impedisce cd/pushd/popd fuori dalla root admin e
    # forza il rientro in root se PWD dovesse uscire dal perimetro.
    rc_content = f"""# Auto-generated by Nexus terminal guard
export NEXUS_TERMINAL_ROOT={json_mod.dumps(str(root))}
export NEXUS_TERMINAL_CWD={json_mod.dumps(initial_cwd)}
__nexus_is_within_root() {{
  local candidate="$1"
  case "$candidate" in
    "$NEXUS_TERMINAL_ROOT"|"$NEXUS_TERMINAL_ROOT"/*) return 0 ;;
    *) return 1 ;;
  esac
}}
__nexus_resolve_path() {{
  local raw="$1"
  if [[ -z "$raw" ]]; then
    raw="$NEXUS_TERMINAL_ROOT"
  fi
  if ! realpath -m -- "$raw" 2>/dev/null; then
    return 1
  fi
}}
__nexus_guarded_cd() {{
  local destination resolved
  destination="${{1:-$NEXUS_TERMINAL_ROOT}}"
  resolved="$(__nexus_resolve_path "$destination")" || {{
    echo "Percorso non valido: $destination"
    return 1
  }}
  if ! __nexus_is_within_root "$resolved"; then
    echo "Operazione negata: non puoi uscire da $NEXUS_TERMINAL_ROOT"
    return 1
  fi
  builtin cd -- "$resolved"
}}
cd() {{
  __nexus_guarded_cd "$@"
}}
pushd() {{
  if [[ "$#" -eq 0 ]]; then
    __nexus_guarded_cd "$NEXUS_TERMINAL_ROOT"
  else
    __nexus_guarded_cd "$1"
  fi
}}
popd() {{
  local before current
  before="$(pwd -P 2>/dev/null || pwd)"
  builtin popd "$@" >/dev/null || return 1
  current="$(pwd -P 2>/dev/null || pwd)"
  if ! __nexus_is_within_root "$current"; then
    echo "Operazione negata: non puoi uscire da $NEXUS_TERMINAL_ROOT"
    builtin cd -- "$before"
    return 1
  fi
  dirs -v
}}
__nexus_enforce_root() {{
  local current
  current="$(pwd -P 2>/dev/null || pwd)"
  if ! __nexus_is_within_root "$current"; then
    echo "Percorso fuori root rilevato, ritorno a $NEXUS_TERMINAL_ROOT"
    builtin cd -- "$NEXUS_TERMINAL_ROOT"
  fi
}}
PROMPT_COMMAND="__nexus_enforce_root${{PROMPT_COMMAND:+;$PROMPT_COMMAND}}"
builtin cd -- "$NEXUS_TERMINAL_CWD"
"""

    fd, rc_path = tempfile.mkstemp(prefix="nexus-term-", suffix=".bashrc")
    os.close(fd)
    with open(rc_path, "w", encoding="utf-8") as handle:
        handle.write(rc_content)

    shell_path = command[0]
    guarded = [shell_path, "--noprofile", "--rcfile", rc_path, "-i"]
    return guarded, rc_path

# --- Shared services (embedding-aware router) ---
embeddings = EmbeddingService()
router = SemanticRouter(embedding_service=embeddings)
providers = ProviderRegistry()
# LLM-based agentic intent classifier (Fase 2). Usa gemini-flash con cache TTL 24h.
# Fallback al classifier keyword (`router`) in caso di timeout/JSON malformato.
agentic_classifier = AgenticIntentClassifier(
    provider_registry=providers,
    fallback_classifier=router,
)

# --- Grafo LangGraph (inizializzazione lazy al primo uso) ---
_agent_graph: object | None = None
_checkpointer: object | None = None

_tool_runner_client: object | None = None
_agent_router_client: object | None = None


def _get_agent_router_client() -> object | None:
    """Singleton `AgentRouterClient`: usato dal `router_node` per consultare
    il Q-Learning router di nexus-orchestrator. Disabilitabile via
    `DISABLE_AGENT_ROUTER=1` o assenza del server gRPC (connessione lazy).
    """
    global _agent_router_client
    if os.environ.get("DISABLE_AGENT_ROUTER") == "1":
        return None
    if _agent_router_client is None:
        try:
            from brain.grpc_clients.agent_router_client import AgentRouterClient
            addr = os.environ.get("AGENT_ROUTER_ADDR", "127.0.0.1:50072")
            _agent_router_client = AgentRouterClient(address=addr)
            logger.info("AgentRouterClient inizializzato su %s", addr)
        except Exception as exc:
            logger.error("AgentRouterClient non disponibile: %s", exc)
            _agent_router_client = None
    return _agent_router_client


def _get_tool_runner_client() -> object | None:
    """Singleton `ToolRunnerClient`: usato dal nodo `tool_dispatch` per
    eseguire i tool contro mcp-core. Se la variabile d'ambiente
    `TOOL_RUNNER_ADDR` non e' impostata (o `DISABLE_TOOL_RUNNER=1`) il
    client non viene istanziato e il grafo torna in modalita' legacy
    single-shot + interrupt_before=[executor].
    """
    global _tool_runner_client
    if os.environ.get("DISABLE_TOOL_RUNNER") == "1":
        return None
    if _tool_runner_client is None:
        try:
            from brain.grpc_clients.tool_runner_client import ToolRunnerClient
            addr = os.environ.get("TOOL_RUNNER_ADDR", "127.0.0.1:50071")
            _tool_runner_client = ToolRunnerClient(address=addr)
            logger.info("ToolRunnerClient inizializzato su %s", addr)
        except Exception as exc:
            logger.error("ToolRunnerClient non disponibile: %s", exc)
            _tool_runner_client = None
    return _tool_runner_client


async def _get_or_init_checkpointer() -> object:
    """Inizializza il checkpointer PostgreSQL asincrono al primo uso.

    Questo deve essere fatto in contesto asincrono per evitare deadlock
    con asyncpg quando il grafo viene compilato e poi invocato da ainvoke().
    """
    global _checkpointer
    if _checkpointer is None:
        from brain.agents.checkpointer import create_checkpointer
        _checkpointer = create_checkpointer()
        # Inizializza il pool asyncpg
        await _checkpointer._ensure_initialized()  # type: ignore[attr-defined]
        logger.info("Checkpointer PostgreSQL inizializzato")
    return _checkpointer


def _get_agent_graph() -> object:
    global _agent_graph
    if _agent_graph is None:
        from brain.agents.graph import create_agent_graph
        _agent_graph = create_agent_graph(
            providers=providers,
            router=router,
            embeddings=embeddings,
            tool_runner=_get_tool_runner_client(),
            agent_router=_get_agent_router_client(),
        )
        logger.info("Grafo LangGraph inizializzato")
    return _agent_graph


# --- FastAPI (debug / health) ---
app = FastAPI(title="Nexus Neural Core", version="0.2.0")

app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_methods=["*"],
    allow_headers=["*"],
)


@app.on_event("startup")
async def startup_event() -> None:
    """Inizializza il checkpointer PostgreSQL asincrono durante l'avvio."""
    logger.info("FastAPI startup: inizializzazione checkpointer PostgreSQL")
    try:
        await _get_or_init_checkpointer()
        logger.info("Checkpointer PostgreSQL pronto")
    except Exception as exc:
        logger.error("Errore durante l'inizializzazione del checkpointer: %s", exc)
        raise


@app.on_event("shutdown")
async def shutdown_event() -> None:
    """Chiude il checkpointer PostgreSQL durante l'arresto."""
    global _checkpointer
    if _checkpointer is not None:
        logger.info("FastAPI shutdown: chiusura checkpointer PostgreSQL")
        try:
            await _checkpointer.aclose()  # type: ignore[attr-defined]
            _checkpointer = None
        except Exception as exc:
            logger.error("Errore durante la chiusura del checkpointer: %s", exc)


class IntentRequest(BaseModel):
    project_id: str
    profile_id: str
    message: str


class AgenticIntentRequest(BaseModel):
    """Schema snello per `/classify-intent-agentic`: solo il messaggio. Niente
    project_id/profile_id perche' la classificazione e' puramente testuale e
    cacheable cross-project."""
    message: str


class CompletionRequest(BaseModel):
    provider: str
    model: str
    prompt: str


class EmbedRequest(BaseModel):
    model: str = ""
    text: str = ""
    texts: list[str] = []


class SearchRequest(BaseModel):
    query: str
    top_k: int = 5


@app.get("/health")
def health() -> dict[str, str]:
    return {"service": "neural-core", "status": "ok", "version": "0.2.0"}


@app.post("/classify-intent")
def classify_intent(body: IntentRequest) -> dict[str, str]:
    return router.classify_intent(body.message)


@app.post("/classify-intent-agentic")
async def classify_intent_agentic(body: AgenticIntentRequest) -> dict[str, object]:
    """Classifier LLM-based con output JSON strutturato (Fase 2).

    Output:
      - intent: come /classify-intent
      - agentic_score: 0..1, probabilita' che il task richieda tool use multi-step
      - requires_tools: bool, hint per la UI / routing
      - complexity: low/medium/high, per scelta tier modello
      - confidence: 0..1, fiducia LLM
      - model_used: modello che ha classificato
      - cached: true se il risultato viene dalla cache TTL 24h
      - fallback_used: true se LLM fallito ed e' stato usato il classifier keyword

    Cache: in-memory TTL 24h, key=sha256(message[:1000]).
    """
    result = await agentic_classifier.classify(body.message)
    return result.to_dict()


@app.get("/classify-intent-agentic/stats")
async def classify_intent_agentic_stats() -> dict[str, object]:
    """Stato cache e configurazione classifier — utile per monitoring."""
    return await agentic_classifier.stats()


@app.post("/route-model")
def route_model(body: IntentRequest) -> dict[str, str]:
    intent = router.classify_intent(body.message)["intent"]
    # Passa anche il message originale: abilita detection task rischiosi
    # (override behavior_mode -> approfondita per verbi distruttivi).
    decision = router.route_model(intent, token_budget=4096, message=body.message)
    return {
        "intent": intent,
        "provider": decision.provider,
        "model": decision.model,
        "rationale": decision.rationale,
        "confidence": str(decision.confidence),
    }


@app.post("/embed")
def embed(body: EmbedRequest) -> dict[str, object]:
    if body.texts:
        vectors = embeddings.embed_batch(body.model, body.texts)
        return {"model": body.model, "vectors": [v.values for v in vectors], "count": len(vectors)}
    vector = embeddings.embed_text(body.model, body.text)
    return {"model": vector.model, "vector": vector.values, "dimensions": len(vector.values)}


@app.post("/search")
def semantic_search(body: SearchRequest) -> dict[str, object]:
    results = embeddings.semantic_search(body.query, body.top_k)
    return {
        "query": body.query,
        "results": [{"id": r.id, "score": r.score, "payload": r.payload} for r in results],
    }


@app.get("/providers/{provider}/models")
def list_models(provider: str) -> dict[str, object]:
    return providers.sync_models(provider)


@app.get("/providers/{provider}/health")
async def provider_health(provider: str) -> dict[str, object]:
    return await providers.test_connection_async(provider)


@app.post("/complete")
async def complete(body: CompletionRequest) -> dict[str, object]:
    result = await providers.generate_completion_async(body.provider, body.model, body.prompt)
    return {
        "provider": result.provider,
        "model": result.model,
        "content": result.content,
        "metadata": result.metadata,
    }


class ReloadSettingsRequest(BaseModel):
    mcp_core_url: str = "http://localhost:4000"




def _apply_dns_override(dns_servers: list[str]) -> None:
    """Override del resolver DNS di sistema con nameserver personalizzati.
    Usa dnspython per risolvere i hostname prima di passarli a socket.getaddrinfo.
    Funziona a livello di processo senza richiedere privilegi root.
    """
    if not dns_servers:
        return
    try:
        import dns.resolver
        import socket as _socket

        resolver = dns.resolver.Resolver(configure=False)
        resolver.nameservers = dns_servers
        resolver.timeout = 5.0
        resolver.lifetime = 10.0

        _original_getaddrinfo = _socket.getaddrinfo

        def _custom_getaddrinfo(host, port, family=0, type=0, proto=0, flags=0):
            # Non fare monkey-patch sugli IP già risolti
            try:
                _socket.inet_pton(_socket.AF_INET, host)
                return _original_getaddrinfo(host, port, family, type, proto, flags)
            except _socket.error:
                pass
            try:
                _socket.inet_pton(_socket.AF_INET6, host)
                return _original_getaddrinfo(host, port, family, type, proto, flags)
            except _socket.error:
                pass
            # Risolvi con il DNS personalizzato
            try:
                answers = resolver.resolve(host, "A")
                ip = str(answers[0])
                return _original_getaddrinfo(ip, port, family, type, proto, flags)
            except Exception:
                # Fallback al DNS di sistema
                return _original_getaddrinfo(host, port, family, type, proto, flags)

        _socket.getaddrinfo = _custom_getaddrinfo
        logger.info("DNS override applicato: %s", dns_servers)

        # Configura il transport DNS globale per httpx (openai, anthropic, mistral, deepseek SDK)
        from brain.providers.dns_transport import configure_dns_transport
        configure_dns_transport(dns_servers)

        # Reset dei client di tutti i provider per forzare nuove connessioni con DNS corretto
        for pname, prov in providers._providers.items():
            if hasattr(prov, '_client'):
                prov._client = None
        logger.info("Client provider resettati dopo DNS override")
    except ImportError:
        logger.warning("dnspython non installato — DNS override non applicato. Installa con: pip install dnspython")
    except Exception as e:
        logger.warning("Errore durante DNS override: %s", e)

def _load_keys_from_db() -> dict[str, object]:
    """Load API keys and enabled flags from PostgreSQL and apply to providers."""
    import os
    import psycopg2

    updated = []
    errors = []

    database_url = os.environ.get("DATABASE_URL", "")
    if not database_url:
        return {"status": "error", "updated": [], "errors": ["DATABASE_URL not set"]}

    try:
        conn = psycopg2.connect(database_url)
        cur = conn.cursor()

        # Carica tutte le chiavi *_api_key dal DB
        cur.execute(
            "SELECT key, value FROM settings WHERE key LIKE %s AND value != ''",
            ("%_api_key",)
        )
        for setting_key, value in cur.fetchall():
            provider_name = setting_key.replace("_api_key", "")
            try:
                p = providers.get_provider(provider_name)
                if p is not None:
                    p._api_key = value
                    p._client = None  # Force reconnect
                    updated.append(provider_name)
            except Exception as e:
                errors.append(f"{provider_name}: {e}")

        # Carica le impostazioni di abilitazione *_enabled dal DB
        cur.execute(
            "SELECT key, value FROM settings WHERE key LIKE %s",
            ("%_enabled",)
        )
        for setting_key, value in cur.fetchall():
            # Filtra solo i provider noti (ignora google_batch_api_enabled ecc.)
            provider_name = setting_key.replace("_enabled", "")
            if providers.get_provider(provider_name) is not None:
                try:
                    providers.set_enabled(provider_name, value.strip().lower() not in ("false", "0", "no"))
                except Exception as e:
                    errors.append(f"{provider_name}_enabled: {e}")


        # Carica ollama_url per il provider locale
        cur.execute("SELECT value FROM settings WHERE key = 'ollama_url'")
        ollama_url_row = cur.fetchone()
        if ollama_url_row and ollama_url_row[0] and ollama_url_row[0].strip():
            ollama_url_val = ollama_url_row[0].strip()
            ollama_prov = providers.get_provider("ollama")
            if ollama_prov is not None:
                ollama_prov._base_url = ollama_url_val
                ollama_prov._client = None  # Force reconnect
                updated.append(f"ollama_url:{ollama_url_val}")

        # Carica network_dns_servers e applica override DNS
        cur.execute("SELECT value FROM settings WHERE key = 'network_dns_servers'")
        dns_row = cur.fetchone()
        if dns_row and dns_row[0] and dns_row[0].strip():
            dns_servers = [s.strip() for s in dns_row[0].split(',') if s.strip()]
            _apply_dns_override(dns_servers)
            updated.append(f"dns:{','.join(dns_servers)}")

        # Carica nexus_external_proxy — imposta NEXUS_PROXY per httpx/requests
        cur.execute("SELECT value FROM settings WHERE key = 'nexus_external_proxy'")
        proxy_row = cur.fetchone()
        if proxy_row is not None:
            proxy_val = (proxy_row[0] or "").strip()
            if proxy_val:
                os.environ["NEXUS_PROXY"] = proxy_val
                # Riconfigura il transport DNS se il proxy prende il sopravvento
                updated.append(f"proxy:{proxy_val}")
            else:
                os.environ.pop("NEXUS_PROXY", None)

        cur.close()
        conn.close()
    except Exception as e:
        errors.append(f"DB connection: {e}")

    return {"status": "ok", "updated": updated, "errors": errors}


@app.post("/reload-settings")
def reload_settings(body: ReloadSettingsRequest) -> dict[str, object]:
    return _load_keys_from_db()


# ── Project Analyzer Agent ──────────────────────────────────────────────────
# Endpoint dedicato all'agente agent.project.analyzer (vedi migrazione 0094):
# carica il prompt dal DB, sostituisce i placeholder col payload del progetto,
# chiama il provider con fallback chain, parsa il JSON risultante.
# Il chiamante e' l'endpoint Rust /api/projects/:id/deep-analyze.
class ProjectAnalyzeRequest(BaseModel):
    project_id: str
    project_name: str
    repo_summary: str = ""
    lang_hint: str = ""
    frameworks_list: list[str] = []
    config_files: list[dict] = []  # [{"path": "...", "content": "...", "truncated": bool}]
    registered_services: list[dict] = []
    # Provider preference (la prima disponibile vince). Se vuota, usa default chain.
    provider_chain: list[dict] = []  # [{"provider":"openai","model":"gpt-4o-mini"}, ...]


class AnalyzerChainUnavailable(Exception):
    """Sollevata quando la chain dei provider per l'analyzer non puo' essere
    letta dal DB (irraggiungibile o nexus_provider_default_model vuota).
    Il caller deve ritornare HTTP 503 con messaggio esplicito invece di
    applicare un fallback hardcoded."""
    pass


def _load_analyzer_provider_chain() -> list[dict]:
    """Carica la chain dei provider per l'analyzer da `nexus_provider_default_model`
    (vedi migrazione 0101) con cache 60s in-process.

    **Niente fallback hardcoded**. Se DB irraggiungibile o tabella vuota,
    solleva `AnalyzerChainUnavailable` con messaggio esplicito.
    """
    global _ANALYZER_CHAIN_CACHE, _ANALYZER_CHAIN_CACHE_TS
    import time
    now = time.time()
    if _ANALYZER_CHAIN_CACHE is not None and (now - _ANALYZER_CHAIN_CACHE_TS) < 60.0:
        return _ANALYZER_CHAIN_CACHE
    try:
        import psycopg2
        db_url = os.environ.get(
            "DATABASE_URL",
            "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable",
        )
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                # Ordine preferenziale per analyzer: economici/veloci prima,
                # capable per ultimo come fallback. L'ordine e' definito
                # dal seed in 0101 ma puo' essere personalizzato.
                cur.execute(
                    "SELECT provider, model_id FROM nexus_provider_default_model "
                    "ORDER BY CASE provider "
                    " WHEN 'openai' THEN 1 WHEN 'google' THEN 2 "
                    " WHEN 'deepseek' THEN 3 WHEN 'mistral' THEN 4 "
                    " WHEN 'anthropic' THEN 5 ELSE 99 END"
                )
                rows = cur.fetchall()
    except Exception as e:
        raise AnalyzerChainUnavailable(
            f"DB irraggiungibile: {e}. Verifica Postgres e migrazione 0101."
        )
    chain = [{"provider": p, "model": m} for (p, m) in rows]
    if not chain:
        raise AnalyzerChainUnavailable(
            "nexus_provider_default_model vuota. Applica la migrazione 0101 e popola la tabella."
        )
    _ANALYZER_CHAIN_CACHE = chain
    _ANALYZER_CHAIN_CACHE_TS = now
    return chain


_ANALYZER_CHAIN_CACHE: list[dict] | None = None
_ANALYZER_CHAIN_CACHE_TS: float = 0.0


def _load_project_analyzer_prompt() -> str | None:
    """Carica il template del prompt agent.project.analyzer dal DB.
    Ritorna None se non trovato.
    """
    try:
        import psycopg2
        db_url = os.environ.get("DATABASE_URL", "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable")
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT content FROM nexus_prompt_templates "
                    "WHERE key='agent.project.analyzer' AND is_active=TRUE "
                    "ORDER BY version DESC LIMIT 1"
                )
                row = cur.fetchone()
                return row[0] if row else None
    except Exception as e:
        logger.error("Errore caricamento prompt project.analyzer: %s", e)
        return None


def _render_analyzer_prompt(template: str, req: "ProjectAnalyzeRequest") -> str:
    """Sostituisce i placeholder {{...}} del template col payload del progetto.
    I file di config vengono serializzati in JSON compatto e inseriti come stringa.
    """
    config_payload = json_mod.dumps(
        [{"path": f.get("path",""), "content": f.get("content","")[:8000],
          "truncated": f.get("truncated", False)} for f in req.config_files],
        ensure_ascii=False,
    )
    services_payload = json_mod.dumps(req.registered_services, ensure_ascii=False)
    return (template
        .replace("{{lang_hint}}", req.lang_hint or "non determinato")
        .replace("{{frameworks_list}}", ", ".join(req.frameworks_list) if req.frameworks_list else "nessuno rilevato")
        .replace("{{repo_summary}}", req.repo_summary or f"progetto {req.project_name}")
        .replace("{{config_files_payload}}", config_payload)
        .replace("{{registered_services}}", services_payload)
    )


def _extract_json_block(text: str) -> dict | None:
    """Estrae il primo blocco JSON valido dal testo.
    Tollera fence markdown ``` o testo prima/dopo.
    """
    # Strip markdown fences se presenti
    cleaned = text.strip()
    if cleaned.startswith("```"):
        # rimuovi prima e ultima riga (```json ... ```)
        lines = cleaned.split("\n")
        if len(lines) >= 3:
            cleaned = "\n".join(lines[1:-1] if lines[-1].strip().startswith("```") else lines[1:])
    # Trova primo { e bilancia
    start = cleaned.find("{")
    if start == -1:
        return None
    depth = 0
    in_str = False
    escape = False
    for i in range(start, len(cleaned)):
        ch = cleaned[i]
        if escape:
            escape = False
            continue
        if ch == "\\" and in_str:
            escape = True
            continue
        if ch == '"':
            in_str = not in_str
            continue
        if in_str:
            continue
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                try:
                    return json_mod.loads(cleaned[start:i+1])
                except Exception:
                    return None
    return None


@app.post("/agent/project-analyze")
async def project_analyze(body: ProjectAnalyzeRequest) -> dict[str, object]:
    """Esegue l'agente agent.project.analyzer su un progetto.

    Pipeline:
      1. Carica template prompt da DB.
      2. Sostituisce placeholder col payload.
      3. Tenta i provider in ordine di preferenza (fallback chain).
      4. Parsa JSON dalla risposta.
      5. Ritorna {insights, model_used, duration_ms, status}.
    """
    started = time.time()
    template = _load_project_analyzer_prompt()
    if not template:
        return {
            "status": "failed",
            "error": "prompt agent.project.analyzer non trovato in DB",
            "insights": None, "model_used": None, "duration_ms": 0,
        }

    rendered = _render_analyzer_prompt(template, body)
    # Carica chain dal DB (nexus_provider_default_model, cache 60s).
    # Errore esplicito 503 se DB down o tabella vuota — niente fallback hardcoded.
    if body.provider_chain:
        chain = body.provider_chain
    else:
        try:
            chain = _load_analyzer_provider_chain()
        except AnalyzerChainUnavailable as e:
            return {
                "status": "failed",
                "error": str(e),
                "duration_ms": 0,
                "model_used": None,
                "insights": {},
            }

    last_error = None
    for entry in chain:
        prov = entry.get("provider", "")
        mdl  = entry.get("model", "")
        if not prov or not mdl:
            continue
        try:
            # Riusa la stessa pipeline del /complete
            result = await providers.generate_completion_async(prov, mdl, rendered)
            content = (result.content or "").strip()
            if not content:
                last_error = f"{prov}/{mdl}: risposta vuota"
                continue
            parsed = _extract_json_block(content)
            if parsed is None:
                last_error = f"{prov}/{mdl}: output non parsabile come JSON"
                # prova provider successivo
                continue
            return {
                "status": "completed",
                "insights": parsed,
                "model_used": f"{prov}/{mdl}",
                "duration_ms": int((time.time() - started) * 1000),
                "raw_length": len(content),
            }
        except Exception as e:
            err_str = str(e)
            last_error = f"{prov}/{mdl}: {err_str[:200]}"
            logger.warning("project_analyze fallback su provider successivo (%s/%s): %s", prov, mdl, err_str[:200])
            continue

    return {
        "status": "failed",
        "error": last_error or "nessun provider disponibile",
        "insights": None,
        "model_used": None,
        "duration_ms": int((time.time() - started) * 1000),
    }


# ── Batch API (Anthropic Messages Batches) ─────────────────────────────────
class BatchAnalyzeRequest(BaseModel):
    requests: list[dict]  # [{"custom_id": str, "system": str, "prompt": str}]
    model: str | None = None  # Risolto da nexus_purpose_model 'anthropic_batch' se None
    max_tokens: int = 4096


@app.post("/batch-analyze/submit")
async def batch_analyze_submit(body: BatchAnalyzeRequest) -> dict[str, str]:
    # Ricaricare le credenziali dal DB per assicurarsi che siano aggiornate
    _load_keys_from_db()

    from brain.providers.anthropic_batch import AnthropicBatchClient
    batch_id = await AnthropicBatchClient().submit_batch(body.requests, body.model, body.max_tokens)
    return {"batch_id": batch_id}


@app.get("/batch-analyze/{batch_id}/status")
async def batch_analyze_status(batch_id: str) -> dict[str, object]:
    from brain.providers.anthropic_batch import AnthropicBatchClient
    return await AnthropicBatchClient().poll_status(batch_id)


@app.get("/batch-analyze/{batch_id}/results")
async def batch_analyze_results(batch_id: str) -> list:
    from brain.providers.anthropic_batch import AnthropicBatchClient
    return await AnthropicBatchClient().get_results(batch_id)


# ── LangGraph Agent Endpoints ─────────────────────────────────────────────────


class AgentRunRequest(BaseModel):
    thread_id: str
    prompt: str
    behavior_mode: str = "bilanciata"
    # Modalita' agent-loop: se `tools_json` e' non vuoto, il grafo usa
    # `generate_agent_turn` e itera su tool_dispatch fino a end_turn.
    tools_json: list[dict] | None = None
    system_text: str = ""
    session_id: str | None = None
    provider_override: str | None = None
    model_override: str | None = None
    # Nome del profilo agente (core/github/specialized/general). Se None,
    # il router sceglie un profilo a partire dall'intent; "none" disabilita.
    profile_name: str | None = None
    conversation_history: list[dict] | None = None
    run_id: str | None = None


class AgentFeedbackRequest(BaseModel):
    score: float


@app.post("/agent/run")
async def agent_run(body: AgentRunRequest) -> dict[str, object]:
    """Avvia un'esecuzione dell'agent LangGraph.

    Il grafo si ferma prima di `executor` (human-in-the-loop).
    Risponde con status "pending_approval" finché non si chiama /agent/approve.

    Nel response completato include le metriche estese: token, costo, latency.
    """
    from langchain_core.messages import AIMessage as _AIMessage
    from langchain_core.messages import HumanMessage as _HumanMessage

    graph = _get_agent_graph()
    config: dict[str, object] = {"configurable": {"thread_id": body.thread_id}}

    history_msgs: list = []
    for msg in (body.conversation_history or []):
        role = msg.get("role", "user")
        content = msg.get("content", "")
        if not content:
            continue
        if role == "assistant":
            history_msgs.append(_AIMessage(content=content))
        else:
            history_msgs.append(_HumanMessage(content=content))

    initial_state = {
        "messages": history_msgs + [_HumanMessage(content=body.prompt)],
        "behavior_mode": body.behavior_mode,
        "thread_id": body.thread_id,
        "iterations": 0,
        "result": None,
        "provider_used": None,
        "model_used": None,
        "feedback_score": None,
        "latency_ms": None,
        "token_usage": None,
        "tools_json": body.tools_json or [],
        "system_text": body.system_text or "",
        "session_id": body.session_id,
        "provider_override": body.provider_override,
        "model_override": body.model_override,
        "profile_name": body.profile_name,
        "pending_tool_uses": [],
        "stop_reason": None,
        "approved": False,
        "prompt_tokens": None,
        "completion_tokens": None,
        "cache_creation_tokens": None,
        "cache_read_tokens": None,
        "total_tokens": None,
        "total_cost_usd": None,
        "cache_hit_rate": None,
        "temperature": None,
        "top_p": None,
        "created_at": None,
        "completed_at": None,
    }
    try:
        result = await graph.ainvoke(initial_state, config=config)  # type: ignore[union-attr]
        # Skip get_state() se non abbiamo un checkpointer (causa deadlock AsyncSqliteSaver)
        next_nodes = None
        try:
            next_nodes = graph.get_state(config).next  # type: ignore[union-attr]
        except Exception:
            pass
        if next_nodes:
            return {
                "status": "pending_approval",
                "thread_id": body.thread_id,
                "next": list(next_nodes),
                "user_intent": result.get("user_intent"),
                "task_type": result.get("task_type"),
                "routing_mode": result.get("behavior_mode"),
            }
        return {
            "status": "completed",
            "thread_id": body.thread_id,
            "result": result.get("result"),
            "provider_used": result.get("provider_used"),
            "model_used": result.get("model_used"),
            "latency_ms": result.get("latency_ms"),
            "usage": {
                "promptTokens": result.get("prompt_tokens") or 0,
                "completionTokens": result.get("completion_tokens") or 0,
                "cacheCreationTokens": result.get("cache_creation_tokens") or 0,
                "cacheReadTokens": result.get("cache_read_tokens") or 0,
                "totalTokens": result.get("total_tokens") or 0,
            },
            "totalCostUsd": result.get("total_cost_usd") or 0.0,
            "cacheHitRate": result.get("cache_hit_rate") or 0.0,
            "temperature": result.get("temperature"),
            "topP": result.get("top_p"),
            "createdAt": result.get("created_at"),
            "completedAt": result.get("completed_at"),
        }
    except Exception as exc:
        logger.error("agent_run error: %s", exc)
        return {"status": "error", "detail": str(exc)}


@app.post("/agent/approve/{thread_id}")
async def agent_approve(thread_id: str) -> dict[str, object]:
    """Riprende l'esecuzione dell'agent dal checkpoint (human approval).

    Include metriche estese nel response.
    """
    graph = _get_agent_graph()
    config: dict[str, object] = {"configurable": {"thread_id": thread_id}}
    try:
        result = await graph.ainvoke(None, config=config)  # type: ignore[union-attr]
        return {
            "status": "completed",
            "thread_id": thread_id,
            "result": result.get("result"),
            "provider_used": result.get("provider_used"),
            "model_used": result.get("model_used"),
            "latency_ms": result.get("latency_ms"),
            "token_usage": result.get("token_usage"),
            "usage": {
                "promptTokens": result.get("prompt_tokens") or 0,
                "completionTokens": result.get("completion_tokens") or 0,
                "cacheCreationTokens": result.get("cache_creation_tokens") or 0,
                "cacheReadTokens": result.get("cache_read_tokens") or 0,
                "totalTokens": result.get("total_tokens") or 0,
            },
            "totalCostUsd": result.get("total_cost_usd") or 0.0,
            "cacheHitRate": result.get("cache_hit_rate") or 0.0,
            "temperature": result.get("temperature"),
            "topP": result.get("top_p"),
            "createdAt": result.get("created_at"),
            "completedAt": result.get("completed_at"),
        }
    except Exception as exc:
        logger.error("agent_approve error: %s", exc)
        return {"status": "error", "detail": str(exc)}


@app.get("/agent/state/{thread_id}")
async def agent_state(thread_id: str) -> dict[str, object]:
    """Recupera lo snapshot di stato del grafo per un thread."""
    graph = _get_agent_graph()
    config: dict[str, object] = {"configurable": {"thread_id": thread_id}}
    try:
        snapshot = graph.get_state(config)  # type: ignore[union-attr]
        return {
            "thread_id": thread_id,
            "next": list(snapshot.next) if snapshot.next else [],
            "values": {
                k: v for k, v in (snapshot.values or {}).items()
                if k not in ("messages",)
            },
        }
    except Exception as exc:
        logger.error("agent_state error: %s", exc)
        return {"status": "error", "detail": str(exc)}


@app.post("/agent/feedback/{thread_id}")
async def agent_feedback(thread_id: str, body: AgentFeedbackRequest) -> dict[str, object]:
    """Registra il feedback utente per l'ultima interazione di un thread."""
    try:
        from brain.memory.storage import LocalLearningStorage
        from brain.agents.checkpointer import get_memory_db_path

        storage = LocalLearningStorage(db_path=get_memory_db_path())
        updated = storage.update_feedback(thread_id, body.score)
        return {"thread_id": thread_id, "updated": updated, "score": body.score}
    except Exception as exc:
        logger.error("agent_feedback error: %s", exc)
        return {"status": "error", "detail": str(exc)}


@app.get("/agent/stats")
async def agent_stats() -> dict[str, object]:
    """Restituisce statistiche aggregate sulle interazioni per tipo di task."""
    try:
        from brain.memory.storage import LocalLearningStorage
        from brain.agents.checkpointer import get_memory_db_path

        storage = LocalLearningStorage(db_path=get_memory_db_path())
        return {"stats": storage.get_task_stats()}
    except Exception as exc:
        logger.error("agent_stats error: %s", exc)
        return {"status": "error", "detail": str(exc)}


# ── Streaming token (SSE) ──────────────────────────────────────────────────
class AgentTurnStreamRequest(BaseModel):
    provider: str
    model: str
    messages_json: str
    tools_json: str
    max_tokens: int = 8192
    system_text: str = ""


@app.post("/agent-turn/stream")
async def agent_turn_stream(body: AgentTurnStreamRequest) -> StreamingResponse:
    import json as _json

    async def generate():
        try:
            # Ricaricare le credenziali dal DB per assicurarsi che siano aggiornate
            _load_keys_from_db()

            prov = providers.get_provider(body.provider)
            if prov is None:
                yield f"data: {_json.dumps({'type': 'error', 'message': f'Provider {body.provider} non trovato'})}\n\n"
                return
            if not hasattr(prov, "generate_agent_turn_stream"):
                yield f"data: {_json.dumps({'type': 'error', 'message': f'Provider {body.provider} non supporta lo streaming'})}\n\n"
                return
            messages = _json.loads(body.messages_json)
            tools = _json.loads(body.tools_json)
            async for chunk in prov.generate_agent_turn_stream(
                body.model, messages, tools, body.max_tokens, body.system_text
            ):
                yield f"data: {_json.dumps(chunk)}\n\n"
        except Exception as exc:
            yield f"data: {_json.dumps({'type': 'error', 'message': str(exc)})}\n\n"

    return StreamingResponse(generate(), media_type="text/event-stream")


# ── Streaming del grafo LangGraph (agent-loop) ─────────────────────────────
@app.post("/agent/run/stream")
async def agent_run_stream(body: AgentRunRequest) -> StreamingResponse:
    """Esegue il grafo agent streamando eventi SSE ad ogni transizione.

    Event types emessi:
      - assistant_delta : contenuto testuale parziale dell'assistant
      - tool_use        : l'LLM ha richiesto un tool (name, input, tool_use_id)
      - tool_result     : output del ToolRunner (tool_use_id, content, is_error)
      - end_turn        : il modello ha terminato (stop_reason=end_turn)
      - error           : errore di esecuzione
    """
    import json as _json

    from langchain_core.messages import AIMessage as _AIMessage
    from langchain_core.messages import HumanMessage as _HumanMessage

    graph = _get_agent_graph()
    config: dict[str, object] = {"configurable": {"thread_id": body.thread_id}}

    history_msgs: list = []
    for msg in (body.conversation_history or []):
        role = msg.get("role", "user")
        content = msg.get("content", "")
        if not content:
            continue
        if role == "assistant":
            history_msgs.append(_AIMessage(content=content))
        else:
            history_msgs.append(_HumanMessage(content=content))

    initial_state = {
        "messages": history_msgs + [_HumanMessage(content=body.prompt)],
        "behavior_mode": body.behavior_mode,
        "thread_id": body.thread_id,
        "iterations": 0,
        "tools_json": body.tools_json or [],
        "system_text": body.system_text or "",
        "session_id": body.session_id,
        "provider_override": body.provider_override,
        "model_override": body.model_override,
        "profile_name": body.profile_name,
        "pending_tool_uses": [],
        "stop_reason": None,
        "approved": False,
    }

    async def generate():
        # Accumulatori token/costo tra tutte le iterazioni dell'agente
        acc_prompt_tokens = 0
        acc_completion_tokens = 0
        acc_total_tokens = 0
        acc_total_cost = 0.0
        end_turn_emitted = False
        done_emitted = False
        # Metadata routing (B5): catturati dal nodo router per propagare a Rust
        nexus_task_type: str | None = None
        nexus_agent_type: str | None = None
        try:
            # Heartbeat: ogni attesa di evento ha un timeout di 30s.
            # Se il brain e' in elaborazione senza produrre output (tool lento,
            # LLM streaming lento, attesa gRPC), emette un ping SSE per segnalare
            # a mcp-core Rust che il run e' ancora attivo.
            # Cosi' mcp-core puo' usare un timeout per-silence (120s) invece del
            # timeout monolitico fisso sulla connessione SSE.
            _aiter = graph.astream(  # type: ignore[union-attr]
                initial_state, config=config, stream_mode="updates"
            ).__aiter__()
            while True:
                try:
                    event = await asyncio.wait_for(_aiter.__anext__(), timeout=30.0)
                except asyncio.TimeoutError:
                    # Brain in elaborazione, nessun evento negli ultimi 30s.
                    # Emetti ping per segnalare attivita' a mcp-core.
                    yield 'data: {"type":"ping"}\n\n'
                    continue
                except StopAsyncIteration:
                    break
                # event = {"<node_name>": <delta_dict>}
                # Flag per rilevare se questo evento e' il `learner_node`
                # (terminale del graph). In tal caso, dopo aver processato
                # tutti i delta emettiamo immediatamente `done` invece di
                # affidarci a StopAsyncIteration che a volte tarda.
                _learner_seen = False
                for node, delta in event.items():
                    if node == "learner":
                        _learner_seen = True
                    if not isinstance(delta, dict):
                        continue
                    if node == "router":
                        # Cattura metadata routing (B5 fix: propagazione nexus_task_type/agent_type)
                        if delta.get("user_intent"):
                            nexus_task_type = delta["user_intent"]
                        if delta.get("profile_name"):
                            nexus_agent_type = delta["profile_name"]
                    elif node == "executor":
                        # Accumula token/costo da ogni chiamata LLM
                        acc_prompt_tokens += int(delta.get("prompt_tokens") or 0)
                        acc_completion_tokens += int(delta.get("completion_tokens") or 0)
                        acc_total_tokens += int(delta.get("total_tokens") or 0)
                        acc_total_cost += float(delta.get("total_cost_usd") or 0.0)

                        result_text = delta.get("result") or ""
                        if result_text:
                            yield (
                                "data: "
                                + _json.dumps({
                                    "type": "assistant_delta",
                                    "text": result_text,
                                })
                                + "\n\n"
                            )
                        for tu in (delta.get("pending_tool_uses") or []):
                            yield (
                                "data: "
                                + _json.dumps({
                                    "type": "tool_use",
                                    "tool_use_id": tu.get("id"),
                                    "name": tu.get("name"),
                                    "input": tu.get("input"),
                                })
                                + "\n\n"
                            )
                        if delta.get("stop_reason") == "end_turn":
                            end_turn_emitted = True
                            end_turn_payload = {
                                "type": "end_turn",
                                "prompt_tokens": acc_prompt_tokens,
                                "completion_tokens": acc_completion_tokens,
                                "total_tokens": acc_total_tokens,
                                "total_cost": acc_total_cost,
                            }
                            # B5: propaga metadata routing a mcp-core Rust
                            if nexus_task_type:
                                end_turn_payload["nexus_task_type"] = nexus_task_type
                            if nexus_agent_type:
                                end_turn_payload["nexus_agent_type"] = nexus_agent_type
                            yield (
                                "data: "
                                + _json.dumps(end_turn_payload)
                                + "\n\n"
                            )
                    elif node == "tool_dispatch":
                        # L'ultimo HumanMessage aggiunto contiene i tool_result.
                        for msg in (delta.get("messages") or []):
                            extra = getattr(msg, "additional_kwargs", {}) or {}
                            for block in (extra.get("anthropic_content") or []):
                                if isinstance(block, dict) and block.get("type") == "tool_result":
                                    yield (
                                        "data: "
                                        + _json.dumps({
                                            "type": "tool_result",
                                            "tool_use_id": block.get("tool_use_id"),
                                            "content": block.get("content"),
                                            "is_error": bool(block.get("is_error")),
                                        })
                                        + "\n\n"
                                    )
                # Se questo era l'evento del learner_node (ultimo del graph),
                # emettiamo subito `end_turn` (se non gia' emesso) e `done`.
                # Risolve il caso in cui astream() non emette StopAsyncIteration
                # tempestivamente dopo aver consumato l'ultimo nodo del graph.
                if _learner_seen:
                    if not end_turn_emitted and acc_total_tokens > 0:
                        end_turn_emitted = True
                        _final_payload = {
                            "type": "end_turn",
                            "prompt_tokens": acc_prompt_tokens,
                            "completion_tokens": acc_completion_tokens,
                            "total_tokens": acc_total_tokens,
                            "total_cost": acc_total_cost,
                        }
                        if nexus_task_type:
                            _final_payload["nexus_task_type"] = nexus_task_type
                        if nexus_agent_type:
                            _final_payload["nexus_agent_type"] = nexus_agent_type
                        yield "data: " + _json.dumps(_final_payload) + "\n\n"
                    yield 'data: {"type":"done"}\n\n'
                    done_emitted = True
                    break
        except Exception as exc:
            import traceback as _tb
            logger.error("agent_run_stream error: %s\n%s", exc, _tb.format_exc())
            # Classifichiamo l'eccezione per propagare error_class strutturato a mcp-core
            # (vedi crates/mcp-core/src/brain_agent_client.rs::classify_provider_error
            # che lo legge come fonte primaria invece di pattern-matchare la stringa).
            try:
                from brain.providers.error_handler import classify_error as _classify
                _info = _classify(exc, body.provider if hasattr(body, 'provider') else "unknown")
                _err_class = _info.get("stop_reason")
                _retry_after = _info.get("retry_after_seconds")
            except Exception:
                _err_class = None
                _retry_after = None
            _payload = {
                "type": "error",
                "message": str(exc) or repr(exc),
            }
            if _err_class:
                _payload["error_class"] = _err_class
            if _retry_after is not None:
                _payload["retry_after_seconds"] = _retry_after
            yield f"data: {_json.dumps(_payload)}\n\n"

        if not end_turn_emitted and acc_total_tokens > 0:
            yield (
                "data: "
                + _json.dumps({
                    "type": "end_turn",
                    "prompt_tokens": acc_prompt_tokens,
                    "completion_tokens": acc_completion_tokens,
                    "total_tokens": acc_total_tokens,
                    "total_cost": acc_total_cost,
                })
                + "\n\n"
            )

        # Segnale terminale esplicito: mcp-core lo intercetta e chiude lo
        # stream immediatamente invece di attendere il timeout di silenzio
        # SSE (120s). Emesso solo se non gia' inviato in-loop quando il
        # learner_node ha prodotto evento (caso flusso normale piu' rapido).
        if not done_emitted:
            yield 'data: {"type":"done"}\n\n'

    return StreamingResponse(generate(), media_type="text/event-stream")


@app.websocket("/ws/terminal/{session_id}")
async def terminal_ws(websocket: WebSocket, session_id: str):
    """WebSocket terminal scoped to a project session emitted by MCP Core."""
    await websocket.accept()
    token = websocket.query_params.get("token")
    payload = _verify_terminal_token(token)
    if payload is None or payload.get("sid") != session_id:
        await websocket.send_text("[Terminal session non valida]")
        await websocket.close(code=4403)
        return

    cwd = str(payload["cwd"])
    env = os.environ.copy()
    env["TERM"] = "xterm-256color"
    shell_command, rc_path = _prepare_shell_command(payload)

    if POSIX_PTY:
        import subprocess

        master_fd, slave_fd = pty.openpty()
        proc = subprocess.Popen(
            shell_command,
            stdin=slave_fd,
            stdout=slave_fd,
            stderr=slave_fd,
            cwd=cwd,
            env=env,
            preexec_fn=os.setsid,
        )
        os.close(slave_fd)

        # ── Server-side output buffer ──
        import re as _re
        _output_buf: list[bytes] = []
        _output_buf_len = 0
        _max_buf = 16384  # 16KB ring buffer
        _project_id = payload.get("pid", "")
        _db_url = os.environ.get("DATABASE_URL", "")

        def _strip_ansi(s: str) -> str:
            s = _re.sub(r"\x1B\[[0-9;]*[A-Za-z]", "", s)
            s = _re.sub(r"\x1B\][^\x07]*\x07", "", s)
            s = _re.sub(r"\x1B\([A-Z]", "", s)
            s = s.replace("\r", "")
            return s.strip()

        def _flush_output_to_db(exit_code_val=None):
            """Scrive il buffer output nel DB per l'ultimo comando della sessione."""
            if not _db_url:
                return
            try:
                import psycopg2
                raw = b"".join(_output_buf).decode("utf-8", errors="replace")
                clean = _strip_ansi(raw)[-8000:]
                conn = psycopg2.connect(_db_url)
                cur = conn.cursor()
                cur.execute(
                    "UPDATE terminal_commands "
                    "SET full_output = %s, exit_code = %s, finished_at = NOW() "
                    "WHERE id = ("
                    "  SELECT id FROM terminal_commands "
                    "  WHERE session_id = %s AND full_output IS NULL "
                    "  ORDER BY created_at DESC LIMIT 1"
                    ")",
                    (clean, exit_code_val, session_id),
                )
                conn.commit()
                cur.close()
                conn.close()
                logger.debug("_flush_output_to_db: wrote %d chars, exit=%s", len(clean), exit_code_val)
            except Exception as e:
                logger.debug("_flush_output_to_db error: %s", e)

        async def _periodic_flush():
            """Debounce server-side: dopo 5s di output stabile, flush al DB."""
            last_len = 0
            stable_count = 0
            try:
                while proc.poll() is None:
                    await asyncio.sleep(1)
                    cur_len = _output_buf_len
                    if cur_len == last_len:
                        stable_count += 1
                        if stable_count >= 5 and cur_len > 0:
                            _flush_output_to_db(exit_code_val=None)
                            stable_count = 0
                    else:
                        last_len = cur_len
                        stable_count = 0
            except asyncio.CancelledError:
                pass

        async def read_pty():
            nonlocal _output_buf_len
            loop = asyncio.get_event_loop()
            try:
                while proc.poll() is None:
                    ready = await loop.run_in_executor(
                        None, lambda: select.select([master_fd], [], [], 0.1)[0]
                    )
                    if ready:
                        try:
                            output = os.read(master_fd, 4096)
                            if not output:
                                break
                            await websocket.send_bytes(output)
                            # Buffer server-side
                            _output_buf.append(output)
                            _output_buf_len += len(output)
                            while _output_buf_len > _max_buf and _output_buf:
                                removed = _output_buf.pop(0)
                                _output_buf_len -= len(removed)
                        except OSError:
                            break
            except Exception as e:
                logger.debug("read_pty ended: %s", e)
            # Processo terminato: flush finale con exit code
            exit_code = proc.poll()
            _flush_output_to_db(exit_code_val=exit_code)
            if exit_code is not None:
                try:
                    await websocket.send_text(json_mod.dumps({
                        "type": "process_exit",
                        "exitCode": exit_code,
                    }))
                except Exception:
                    pass

        flush_task = asyncio.create_task(_periodic_flush())
        reader_task = asyncio.create_task(read_pty())

        try:
            while True:
                data = await websocket.receive()
                if "bytes" in data:
                    os.write(master_fd, data["bytes"])
                elif "text" in data:
                    text = data["text"]
                    if text.startswith("{"):
                        try:
                            msg = json_mod.loads(text)
                            if msg.get("type") == "resize":
                                winsize = struct.pack("HHHH", msg["rows"], msg["cols"], 0, 0)
                                fcntl.ioctl(master_fd, termios.TIOCSWINSZ, winsize)
                                continue
                        except (json_mod.JSONDecodeError, KeyError):
                            pass
                    os.write(master_fd, text.encode())
        except WebSocketDisconnect:
            pass
        except Exception as e:
            logger.debug("terminal_ws ended: %s", e)
        finally:
            flush_task.cancel()
            reader_task.cancel()
            try:
                os.close(master_fd)
            except OSError:
                pass
            try:
                proc.terminate()
                proc.wait(timeout=2)
            except Exception:
                proc.kill()
            if rc_path:
                try:
                    os.unlink(rc_path)
                except OSError:
                    pass
        return

    proc = await asyncio.create_subprocess_exec(
        *shell_command,
        cwd=cwd,
        env=env,
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.STDOUT,
    )

    async def read_stream():
        try:
            while True:
                chunk = await proc.stdout.read(4096)
                if not chunk:
                    break
                await websocket.send_bytes(chunk)
        except Exception as e:
            logger.debug("read_stream ended: %s", e)

    reader_task = asyncio.create_task(read_stream())

    try:
        while True:
            data = await websocket.receive()
            if "bytes" in data and proc.stdin:
                proc.stdin.write(data["bytes"])
                await proc.stdin.drain()
            elif "text" in data and proc.stdin:
                text = data["text"]
                if text.startswith("{"):
                    try:
                        msg = json_mod.loads(text)
                        if msg.get("type") == "resize":
                            continue
                    except json_mod.JSONDecodeError:
                        pass
                proc.stdin.write(text.encode())
                await proc.stdin.drain()
    except WebSocketDisconnect:
        pass
    except Exception as e:
        logger.debug("terminal_ws ended: %s", e)
    finally:
        reader_task.cancel()
        try:
            if proc.stdin:
                proc.stdin.close()
        except Exception:
            pass
        if proc.returncode is None:
            proc.terminate()
            await proc.wait()
        if rc_path:
            try:
                os.unlink(rc_path)
            except OSError:
                pass


def _start_rest(port: int = 8001) -> None:
    import uvicorn
    uvicorn.run(app, host="0.0.0.0", port=port, log_level="info")


def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s: %(message)s")

    grpc_port = 50051

    # Load API keys from DB at startup
    result = _load_keys_from_db()
    logger.info("Startup key reload: %s", result)

    # Load agent prompts from DB into in-memory registry
    try:
        from brain.agents import prompt_registry
        n_prompts = prompt_registry.load_from_db()
        logger.info("Startup agent prompts loaded: %d", n_prompts)
    except Exception as exc:
        logger.warning("Startup agent prompts load fallito: %s", exc)

    rest_thread = threading.Thread(target=_start_rest, daemon=True)
    rest_thread.start()
    logger.info("FastAPI HTTP server avviato su porta 8001")

    from brain.grpc_server import neural_service
    neural_service.embeddings = embeddings
    neural_service.router = router
    neural_service.providers = providers
    neural_service.serve(port=grpc_port)


if __name__ == "__main__":
    main()
