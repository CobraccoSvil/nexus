"""Stato condiviso e helper di basso livello del Neural Core.

Questo modulo concentra le istanze di servizio (embedding-aware router,
provider registry, classifier agentic), i singleton lazy (grafo LangGraph,
checkpointer PostgreSQL, client gRPC ToolRunner/AgentRouter) e gli helper
condivisi tra i vari gruppi di endpoint (settings cache, sicurezza terminale,
perimetro progetti, reload chiavi/DNS).

Motivazione architetturale: gli endpoint REST vivono in moduli `routes/*`
distinti ma condividono lo stesso stato globale. Per evitare la trappola delle
copie-di-riferimento (in Python `from runtime import providers` copierebbe il
riferimento al momento dell'import e non vedrebbe i riassegnamenti), i moduli
route accedono SEMPRE allo stato via attributo di modulo: `runtime.providers`,
`runtime._get_agent_graph()`, ecc. Cosi' un singolo punto di verita' resta
valido per tutto il processo.
"""
from __future__ import annotations

import base64
import hashlib
import hmac
import json as json_mod
import logging
import os
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

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


_PROJECT_ROOTS_CACHE: tuple[float, set[str]] | None = None
_PROJECT_ROOTS_TTL_SECONDS = 30.0


def _registered_project_roots() -> set[str]:
    """Set delle repository_root_path dei progetti registrati, risolte.

    Fonte di verita': tabella projects (regola G, niente perimetro hardcoded).
    Cache TTL breve: evita una query DB per ogni apertura di terminale.
    Degradazione esplicita (regola H): in caso di errore DB si riusa l'ultima
    cache valida; se non c'e' cache, l'autorizzazione ricade sul perimetro
    _allowed_roots() senza inghiottire l'errore (loggato come warning).
    """
    global _PROJECT_ROOTS_CACHE
    now = time.time()
    if _PROJECT_ROOTS_CACHE and now - _PROJECT_ROOTS_CACHE[0] <= _PROJECT_ROOTS_TTL_SECONDS:
        return _PROJECT_ROOTS_CACHE[1]

    db_url = os.environ.get("DATABASE_URL")
    if not db_url:
        return _PROJECT_ROOTS_CACHE[1] if _PROJECT_ROOTS_CACHE else set()

    roots: set[str] = set()
    try:
        import psycopg2

        conn = psycopg2.connect(db_url)
        try:
            cur = conn.cursor()
            cur.execute(
                "SELECT repository_root_path FROM projects "
                "WHERE repository_root_path IS NOT NULL AND repository_root_path <> ''"
            )
            for (path_value,) in cur.fetchall():
                try:
                    roots.add(str(Path(path_value).expanduser().resolve()))
                except Exception:
                    continue
            cur.close()
        finally:
            conn.close()
    except Exception as exc:
        logger.warning("registered_project_roots: query progetti fallita: %s", exc)
        if _PROJECT_ROOTS_CACHE:
            return _PROJECT_ROOTS_CACHE[1]
        return set()

    _PROJECT_ROOTS_CACHE = (now, roots)
    return roots


def _is_registered_project_root(resolved_root: Path) -> bool:
    """True se il root firmato corrisponde alla root di un progetto registrato."""
    return str(resolved_root) in _registered_project_roots()


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

    # Il root del terminale e' autorizzato se sta dentro il perimetro admin
    # (projects_base_root / PROJECTS_ALLOWED_ROOTS) OPPURE se coincide con la
    # root di un progetto registrato (isolamento per-progetto, regola E): in
    # questo modo i progetti importati fuori da projects_base_root hanno un
    # terminale funzionante senza allargare il perimetro a path arbitrari.
    within_perimeter = any(
        _path_within(resolved_root, allowed_root) for allowed_root in _allowed_roots()
    )
    if not (within_perimeter or _is_registered_project_root(resolved_root)):
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
            # Non passare un address hardcoded (regola G: il DB e' l'unica fonte
            # di verita'). Il costruttore risolve la gerarchia canonica
            # env AGENT_ROUTER_ADDR > settings.agent_router_addr (DB) > default.
            # Il vecchio default qui era '127.0.0.1:50072', porta storica
            # dismessa: mcp-core espone AgentRouter su 50501 (mig 0190/0239),
            # quindi il client falliva sempre con Connection refused.
            _agent_router_client = AgentRouterClient()
            logger.info(
                "AgentRouterClient inizializzato su %s",
                getattr(_agent_router_client, "address", "?"),
            )
        except Exception as exc:
            logger.error("AgentRouterClient non disponibile: %s", exc)
            _agent_router_client = None
    return _agent_router_client


def _get_tool_runner_client() -> object | None:
    """Singleton `ToolRunnerClient`: usato dal nodo `tool_dispatch` per
    eseguire i tool contro mcp-core. Disabilitabile con `DISABLE_TOOL_RUNNER=1`;
    altrimenti il client viene sempre istanziato risolvendo l'indirizzo dal DB.
    """
    global _tool_runner_client
    if os.environ.get("DISABLE_TOOL_RUNNER") == "1":
        return None
    if _tool_runner_client is None:
        try:
            from brain.grpc_clients.tool_runner_client import ToolRunnerClient
            # Non passare un address hardcoded (regola G): il costruttore risolve
            # la gerarchia canonica env TOOL_RUNNER_ADDR >
            # settings.tool_runner_addr (DB) > default. Il vecchio default qui
            # era '127.0.0.1:50071', porta storica dismessa: mcp-core espone il
            # ToolRunner su 50500 (mig 0239), quindi il client falliva sempre con
            # Connection refused e NESSUN tool veniva eseguito -> hollow completion.
            _tool_runner_client = ToolRunnerClient()
            logger.info(
                "ToolRunnerClient inizializzato su %s",
                getattr(_tool_runner_client, "address", "?"),
            )
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
            agentic_classifier=agentic_classifier,
        )
        logger.info("Grafo LangGraph inizializzato")
    return _agent_graph


async def _warmup_google_provider() -> None:
    """Inizializza il client Google genai (Vertex SA + httpx pool) con una
    chiamata sintetica count_tokens. Cold start eliminato per i call utente.

    Best-effort: ogni errore loggato come INFO, mai propagato.
    """
    import asyncio as _aio

    try:
        from brain.providers.google_provider import GoogleProvider

        provider = GoogleProvider()
        ok, reason = provider._is_configured()
        if not ok:
            logger.info("Vertex warmup: provider google non configurato (%s), skip", reason)
            return

        # Regola G: niente nome modello in codice. Risolvi un modello google
        # qualsiasi enabled dal catalog per il warmup (e' una chiamata tecnica
        # di pre-warm del client SDK, non una scelta di routing).
        def _resolve_warmup_model() -> str | None:
            try:
                import psycopg2  # type: ignore[import-untyped]
                from brain.utils.settings_db import get_db_url
                with psycopg2.connect(get_db_url()) as conn:
                    with conn.cursor() as cur:
                        cur.execute(
                            "SELECT model FROM ai_price_catalog "
                            "WHERE provider = 'google' AND is_enabled = TRUE "
                            "ORDER BY is_featured DESC, input_cost_per_million_tokens ASC NULLS LAST "
                            "LIMIT 1"
                        )
                        row = cur.fetchone()
                        return row[0] if row else None
            except Exception as exc:
                logger.info("Vertex warmup: risoluzione modello fallita (%s)", exc)
                return None

        warmup_model = _resolve_warmup_model()
        if not warmup_model:
            logger.info("Vertex warmup: nessun modello google enabled nel catalog, skip")
            return

        def _do_warmup() -> int:
            client = provider._get_client()
            response = client.models.count_tokens(model=warmup_model, contents="warmup")
            return int(getattr(response, "total_tokens", 0))

        loop = _aio.get_running_loop()
        tokens = await loop.run_in_executor(None, _do_warmup)
        logger.info(
            "Vertex warmup OK: client genai pre-inizializzato (model=%s, total_tokens=%d)",
            warmup_model, tokens,
        )
    except Exception as exc:
        logger.info("Vertex warmup: skipped (%s)", exc)


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

    database_url = os.environ.get("DATABASE_URL")
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
