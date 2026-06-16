"""Google provider adapter — dual backend: Gemini API direct + Vertex AI.

CHIAMATE LLM: NON eseguite da questo adapter. Dopo il consolidamento del
trasporto (regola L / ADR 0026) tutte le chiamate (generate / agent turn)
passano dal ``GatewayProvider`` (delega al gateway Rust). Questo adapter resta
costruito SOLO per i metodi NON-chiamata: ``list_models`` (catalog-sync) e
``test_connection`` (health-check admin), piu' il client SDK on-demand
(dual-backend gemini/vertex) che essi usano. Le quirk Google delle chiamate
(conversione Contents, thought_signature round-trip, ThinkingConfig, tool_config
ANY/AUTO, recupero tool-as-text) vivono ora nel gateway Rust
(crates/nexus-gateway/src/providers/google.rs).

Backend selezionato via settings DB `google_provider_backend`:
  - "gemini" (default): genai.Client(api_key=...) → generativelanguage.googleapis.com
  - "vertex": genai.Client(vertexai=True, project=..., location=...) → aiplatform.googleapis.com

Stesso SDK (google-genai), stessi modelli (gemini-*), API differente.
Vertex AI offre: region selection (GDPR), audit log, quota piu' alta, Service Account auth.
Gemini direct: API key semplice, free tier disponibile.

Vedi migrazione 0183 per la lista chiavi DB:
  - google_provider_backend
  - google_vertex_project
  - google_vertex_location
  - google_vertex_credentials_json (Service Account JSON, is_secret=true)
"""
from __future__ import annotations

import asyncio
import json
import logging
import os
import tempfile
from typing import Any

from .base import BaseProvider, ProviderCatalogEntry

logger = logging.getLogger(__name__)


def _read_setting(key: str, default: str = "") -> str:
    """Legge un setting dal DB (best-effort, ritorna default in caso di errore)."""
    try:
        from brain.utils.settings_db import get_setting
        val = get_setting(key)
        return val if val else default
    except Exception as exc:
        logger.debug("Settings DB unreachable per '%s' (%s), uso default", key, exc)
        return default


class GoogleProvider(BaseProvider):
    name = "google"

    def __init__(self) -> None:
        # API key letta dal DB con cache 60s. Vedi api_key_loader.py.
        from .api_key_loader import load_api_key
        self._api_key_provider = lambda: load_api_key(self.name)
        # Cache per-event-loop: id(loop) -> (Client, backend_signature).
        # genai.Client contiene un httpx.AsyncClient legato al loop dove e' stato
        # creato; se il loop cambia o si chiude (reload, shutdown lifespan) il
        # client va ricreato, altrimenti tutte le chiamate falliscono con
        # "Event loop is closed". Inoltre, se la configurazione backend cambia
        # (gemini ↔ vertex, project, location, key/credentials), il client va
        # ricreato per puntare al nuovo backend.
        self._clients_by_loop: dict[int, tuple[Any, str]] = {}
        self._cached_key: str = ""
        # Path temporaneo per Service Account JSON (creato se backend=vertex e
        # le credenziali sono nel DB invece che in GOOGLE_APPLICATION_CREDENTIALS).
        self._vertex_creds_temp_path: str | None = None

    @property
    def _api_key(self) -> str:
        new_key = self._api_key_provider()
        if new_key != self._cached_key:
            self._cached_key = new_key
            self._clients_by_loop.clear()
        return new_key

    @_api_key.setter
    def _api_key(self, value: str) -> None:
        from .api_key_loader import invalidate_cache
        invalidate_cache(self.name)
        self._cached_key = value or ""
        self._clients_by_loop.clear()

    def _resolve_backend_config(self) -> dict[str, str]:
        """Legge la config backend dal DB. Ritorna dict normalizzato.

        Output keys:
          - backend: "gemini" | "vertex"
          - project: GCP project ID (solo vertex)
          - location: GCP region (solo vertex)
          - credentials_json: contenuto Service Account JSON (solo vertex,
            puo' essere vuoto se si usa GOOGLE_APPLICATION_CREDENTIALS env)
        """
        backend = _read_setting("google_provider_backend", "gemini").lower().strip()
        if backend not in ("gemini", "vertex"):
            logger.warning(
                "google_provider_backend='%s' invalido, fallback a gemini", backend,
            )
            backend = "gemini"
        cfg = {"backend": backend, "project": "", "location": "", "credentials_json": ""}
        if backend == "vertex":
            cfg["project"] = _read_setting("google_vertex_project", "").strip()
            cfg["location"] = _read_setting("google_vertex_location", "europe-west4").strip()
            cfg["credentials_json"] = _read_setting("google_vertex_credentials_json", "").strip()
            if not cfg["project"]:
                logger.error(
                    "Vertex backend selezionato ma google_vertex_project vuoto: "
                    "fallback a Gemini direct con API key.",
                )
                cfg["backend"] = "gemini"
        return cfg

    def _backend_signature(self, cfg: dict[str, str]) -> str:
        """Firma stabile della config: se cambia, il client cached va invalidato.

        Il marker `creds_marker` riflette ESCLUSIVAMENTE le credenziali dal DB.
        Niente fallback ad ADC/env: se cambia il SA JSON in DB, la firma cambia
        e il client viene ricreato puntando alle nuove credenziali.
        """
        if cfg["backend"] == "vertex":
            creds_marker = "len=" + str(len(cfg["credentials_json"]))
            return f"vertex:{cfg['project']}:{cfg['location']}:{creds_marker}"
        return f"gemini:{self._cached_key[:8] if self._cached_key else ''}"

    def _setup_vertex_credentials(self, credentials_json: str) -> bool:
        """Scrive il SA JSON dal DB in un file temp e setta GOOGLE_APPLICATION_CREDENTIALS.

        Regola G del CLAUDE.md: il DB e' l'UNICA fonte di verita'. Niente
        fallback ad ADC/GOOGLE_APPLICATION_CREDENTIALS preesistente: se il
        setting `google_vertex_credentials_json` e' vuoto o invalido, il
        Vertex SDK NON deve ereditare credenziali dal filesystem o
        dall'environment del processo (es. SA mountato da k8s, gcloud auth).
        Sempre e solo dal DB.

        Ritorna True se il setup e' andato a buon fine, False altrimenti
        (in tal caso _is_configured fallira' e generate*() ritorneranno errore
        esplicito).
        """
        # Pulisci sempre l'env var preesistente: il processo brain potrebbe
        # essere stato avviato con GOOGLE_APPLICATION_CREDENTIALS settata dal
        # sistemd/docker, vogliamo isolare la sorgente di verita' al DB.
        os.environ.pop("GOOGLE_APPLICATION_CREDENTIALS", None)
        # Pulisci eventuale file temp precedente.
        if self._vertex_creds_temp_path:
            try:
                os.remove(self._vertex_creds_temp_path)
            except OSError:
                pass
            self._vertex_creds_temp_path = None

        if not credentials_json:
            logger.error(
                "Vertex backend richiede google_vertex_credentials_json in DB. "
                "Setting vuoto: il client NON puo' essere istanziato.",
            )
            return False
        # Valida JSON e fields minimi del Service Account.
        try:
            sa_data = json.loads(credentials_json)
        except json.JSONDecodeError as exc:
            logger.error(
                "google_vertex_credentials_json non e' JSON valido: %s. "
                "Il Vertex client NON sara' istanziato.",
                exc,
            )
            return False
        required_fields = ("type", "project_id", "private_key", "client_email")
        missing = [f for f in required_fields if f not in sa_data]
        if missing:
            logger.error(
                "google_vertex_credentials_json incompleto: mancano %s. "
                "Aspettato Service Account JSON (campi: %s).",
                missing, required_fields,
            )
            return False
        if sa_data.get("type") != "service_account":
            logger.error(
                "google_vertex_credentials_json type='%s' non e' 'service_account'.",
                sa_data.get("type"),
            )
            return False

        fd, path = tempfile.mkstemp(prefix="nexus_vertex_sa_", suffix=".json")
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                f.write(credentials_json)
            os.chmod(path, 0o600)
            os.environ["GOOGLE_APPLICATION_CREDENTIALS"] = path
            self._vertex_creds_temp_path = path
            logger.info(
                "Vertex Service Account caricato dal DB e materializzato in %s (perms 600, sa=%s)",
                path, sa_data.get("client_email", "?"),
            )
            return True
        except OSError as exc:
            logger.error("Impossibile scrivere SA JSON: %s", exc)
            return False

    def _get_client(self) -> Any:
        # Risolvi il loop corrente; se chiamato fuori da contesto async
        # (caso anomalo, ma per sicurezza) ne crea uno temporaneo.
        try:
            loop = asyncio.get_running_loop()
        except RuntimeError:
            loop = asyncio.new_event_loop()
        loop_id = id(loop)
        # Config backend corrente.
        cfg = self._resolve_backend_config()
        signature = self._backend_signature(cfg)
        # Se il loop cachato e' diverso/chiuso, oppure la signature backend e' cambiata,
        # scarta i client stale.
        if loop_id not in self._clients_by_loop or loop.is_closed():
            stale = [k for k in self._clients_by_loop.keys() if k != loop_id]
            for k in stale:
                self._clients_by_loop.pop(k, None)
        cached = self._clients_by_loop.get(loop_id)
        if cached is not None and cached[1] != signature:
            logger.info(
                "Google client signature cambiata (%s -> %s), ricreo client",
                cached[1], signature,
            )
            self._clients_by_loop.pop(loop_id, None)
            cached = None
        if cached is None:
            from google import genai  # type: ignore[import]
            from .dns_transport import get_global_dns_transport
            transport = get_global_dns_transport()
            if transport is not None:
                # Google genai non supporta http_client custom; usiamo monkey-patch socket
                import socket as _socket
                import dns.resolver as _dns
                _resolver = _dns.Resolver(configure=False)
                _resolver.nameservers = transport._dns_resolver.nameservers
                _original = getattr(_socket, '_orig_getaddrinfo', _socket.getaddrinfo)
                _socket._orig_getaddrinfo = _original
                def _custom_gai(host, port, family=0, type=0, proto=0, flags=0):
                    # IMPORTANTE: getaddrinfo puo' essere chiamato con host=bytes
                    # (es. da urllib3/httpx in alcuni code path); inet_pton invece
                    # accetta SOLO str. Senza la coercion qui sotto si rompe con
                    # "TypeError: inet_pton() argument 2 must be str, not bytes"
                    # (vedi bug 6 del test E2E redemptor).
                    host_str = host.decode('ascii', errors='ignore') if isinstance(host, (bytes, bytearray)) else host
                    for af in (_socket.AF_INET, _socket.AF_INET6):
                        try:
                            _socket.inet_pton(af, host_str)
                            return _original(host, port, family, type, proto, flags)
                        except (_socket.error, TypeError):
                            pass
                    try:
                        return _original(str(_resolver.resolve(host_str, 'A')[0]), port, family, type, proto, flags)
                    except Exception:
                        return _original(host, port, family, type, proto, flags)
                _socket.getaddrinfo = _custom_gai
            # Istanzia client per il backend scelto.
            if cfg["backend"] == "vertex":
                # Setup credenziali dal DB (regola G). Se fallisce, NON tentiamo
                # ADC/env fallback: solleviamo per fail-fast cosi' generate*()
                # ritornano errore esplicito invece di chiamare Google con
                # credenziali ereditate inaspettate dall'environment.
                if not self._setup_vertex_credentials(cfg["credentials_json"]):
                    raise RuntimeError(
                        "Vertex backend selezionato ma credenziali DB invalide/mancanti. "
                        "Vedi log per dettagli. Compila google_vertex_credentials_json in Admin."
                    )
                client = genai.Client(
                    vertexai=True,
                    project=cfg["project"],
                    location=cfg["location"],
                )
                logger.info(
                    "Google provider: backend=vertex project=%s location=%s",
                    cfg["project"], cfg["location"],
                )
            else:
                client = genai.Client(api_key=self._api_key)
                logger.info("Google provider: backend=gemini (API key direct)")
            self._clients_by_loop[loop_id] = (client, signature)
        return self._clients_by_loop[loop_id][0]

    async def aclose_current_loop_clients(self) -> None:
        """Chiude il client genai (httpx.AsyncClient) legato al loop corrente,
        rimuovendolo dalla cache. Va chiamato dal wrapper sync PRIMA di chiudere
        il loop, per evitare 'Event loop is closed' nel finalizer di httpx."""
        try:
            loop_id = id(asyncio.get_running_loop())
        except RuntimeError:
            return
        entry = self._clients_by_loop.pop(loop_id, None)
        if not entry:
            return
        client = entry[0]
        try:
            await client.aio.aclose()
        except Exception:
            pass

    def _is_configured(self) -> tuple[bool, str]:
        """Verifica che il backend selezionato abbia config sufficiente.

        Regola G del CLAUDE.md: TUTTA la config viene dal DB, niente env var,
        niente ADC, niente fallback nascosti. Se il DB manca dei campi
        richiesti, il provider NON deve funzionare.

        Ritorna (ok, motivo). Se ok=False, le chiamate generate*() devono
        ritornare un error result immediato senza chiamare il client.
        """
        cfg = self._resolve_backend_config()
        if cfg["backend"] == "vertex":
            if not cfg["project"]:
                return False, "Vertex backend: setting 'google_vertex_project' vuoto in DB"
            if not cfg["location"]:
                return False, "Vertex backend: setting 'google_vertex_location' vuoto in DB"
            if not cfg["credentials_json"]:
                return False, (
                    "Vertex backend: setting 'google_vertex_credentials_json' vuoto in DB. "
                    "Incolla il contenuto del Service Account JSON in Admin > Settings."
                )
            # Validazione JSON minima (firma SA): se non passa, _setup_vertex_credentials
            # loggera' l'errore al prossimo _get_client e ritornera' False.
            try:
                sa = json.loads(cfg["credentials_json"])
                if sa.get("type") != "service_account":
                    return False, "Vertex backend: SA JSON in DB non e' di tipo 'service_account'"
            except json.JSONDecodeError:
                return False, "Vertex backend: SA JSON in DB non e' JSON valido"
            return True, ""
        # Backend gemini: serve API key
        if not self._api_key:
            return False, "Gemini backend: API key non configurata in settings"
        return True, ""

    def list_models(self) -> list[ProviderCatalogEntry]:
        # Lista modelli letta da DB (ai_price_catalog) con cache 60s.
        # Niente fallback hardcoded.
        from .catalog_loader import load_provider_catalog
        return load_provider_catalog(self.name)

    async def test_connection(self) -> dict[str, Any]:
        ok, reason = self._is_configured()
        if not ok:
            return {"provider": self.name, "status": "not_configured", "reason": reason}
        try:
            client = self._get_client()
            # Usa models.list() (non fatturata) invece di generate_content per evitare
            # di consumare crediti ad ogni health-check.
            # google-genai >= 1.x: aio.models.list() e' una coroutine, non un async iterator.
            await client.aio.models.list()
            return {"provider": self.name, "status": "ready"}
        except Exception as e:
            from .error_handler import classify_error
            info = classify_error(e, self.name)
            return {"provider": self.name, "status": "error", "reason": info["message"], "error_class": info["stop_reason"]}
