"""Google provider — dual backend: Gemini API direct + Vertex AI.

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
from typing import Any, AsyncIterator

from .base import BaseProvider, ProviderCatalogEntry, ProviderResult
from .error_handler import format_error_result

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

    async def generate(self, model: str, prompt: str, **kwargs: Any) -> ProviderResult:
        ok, reason = self._is_configured()
        if not ok:
            return ProviderResult(
                provider=self.name, model=model,
                content=f"[Google provider non configurato: {reason}]",
                metadata={"error": "missing_config", "reason": reason},
            )
        try:
            from google.genai import types  # type: ignore[import]
            client = self._get_client()
            response = await client.aio.models.generate_content(
                model=model,
                contents=prompt,
                config=types.GenerateContentConfig(
                    max_output_tokens=kwargs.get("max_tokens", 4096),
                    temperature=kwargs.get("temperature", 0.7),
                ),
            )
            prompt_tokens = 0
            completion_tokens = 0
            if response.usage_metadata:
                prompt_tokens = response.usage_metadata.prompt_token_count or 0
                completion_tokens = response.usage_metadata.candidates_token_count or 0
            return ProviderResult(
                provider=self.name,
                model=model,
                content=response.text or "",
                metadata={
                    "usage": {
                        "prompt_tokens": prompt_tokens,
                        "completion_tokens": completion_tokens,
                    },
                },
            )
        except Exception as e:
            meta = format_error_result(e, self.name, model)
            return ProviderResult(
                provider=self.name, model=model,
                content=f"[Error: {meta['error']}]",
                metadata=meta,
            )

    async def generate_agent_turn(
        self,
        model: str,
        messages: list[dict],
        tools: list[dict],
        max_tokens: int = 4096,
        system_text: str = "",
        temperature: float = 0.7,
        force_tool_choice: bool | None = None,
    ) -> ProviderResult:
        """Turno agente con function calling Google Gemini, normalizzato al formato Anthropic."""
        ok, reason = self._is_configured()
        if not ok:
            return ProviderResult(
                provider=self.name, model=model,
                content=f"[Google provider non configurato: {reason}]",
                metadata={"error": "missing_config", "reason": reason},
            )
        try:
            from google.genai import types  # type: ignore[import]
            import json as _json

            client = self._get_client()

            # Capability DB-driven (regola G): max_tokens clampato al tetto del
            # modello. Degrada ai parametri richiesti se la riga manca.
            cap = None
            try:
                from .capability_loader import load_capability
                from .adapter_base import resolve_max_tokens
                cap = load_capability(self.name, model)
                max_tokens = resolve_max_tokens(cap, max_tokens)
            except Exception as _cap_err:
                logger.warning(
                    "capability %s/%s non disponibile (%s): uso parametri richiesti",
                    self.name, model, _cap_err,
                )
                cap = None

            # Converti messaggi Anthropic -> Google genai Contents
            contents = _convert_messages_to_google(messages)

            # Converti tool definitions Anthropic -> Google FunctionDeclaration
            google_tools = None
            if tools:
                func_decls = []
                for t in tools:
                    schema = t.get("input_schema", {"type": "object", "properties": {}})
                    # Rimuovi chiavi non supportate da Google (additionalProperties, $schema)
                    clean_schema = _clean_schema_for_google(schema)
                    func_decls.append(types.FunctionDeclaration(
                        name=t["name"],
                        description=t.get("description", ""),
                        parameters=clean_schema,
                    ))
                google_tools = [types.Tool(function_declarations=func_decls)]

            # I modelli "thinking" (gemini-2.0/2.5-flash-thinking-exp, gemini-2.5-pro-exp,
            # gemini-2.5-flash, gemini-2.5-pro) usano un budget di reasoning interno.
            # - Temperature ignorato dal provider: settiamo None per evitare warning.
            # - include_thoughts=True espone i "thoughts" del modello insieme alla
            #   risposta, cosi' il brain/UI puo' mostrare il ragionamento.
            #   Senza questo, il reasoning resta interno e l'utente vede solo
            #   la risposta finale ("non vedo extended thinking" feedback utente).
            # ADR 0025: nei loop agentici (tool presenti) i dual-mode
            # 'disable_for_tools' girano in NON-THINKING: su Gemini 2.5 il thinking
            # col function calling forzato produce MALFORMED_FUNCTION_CALL (mig 0274).
            # Disabilitiamo il thinking (nessun ThinkingConfig, budget off).
            # Fonte UNICA del comportamento thinking: agentic_thinking_policy dal DB
            # (ADR 0024/0025), MAI il nome del modello. Bug storico: derivare
            # _is_thinking da substring "gemini-2.5-flash" marcava come thinking
            # anche gemini-2.5-flash-lite (policy='none', NON-thinking) -> gli si
            # inviava ThinkingConfig(thinking_budget) fuori range (512-24576) ->
            # 400 INVALID_ARGUMENT ad ogni chiamata.
            try:
                from .capability_loader import load_capability
                _policy_tools = load_capability(self.name, model).agentic_thinking_policy
            except Exception:
                _policy_tools = "none"
            _force_non_thinking_tools = bool(google_tools) and _policy_tools == "disable_for_tools"
            # Thinking attivo SOLO se la policy lo prevede (native sempre; dual-mode
            # 'disable_for_tools' solo SENZA tool, cioe' in chat) e non stiamo
            # forzando il non-thinking per i tool. policy 'none'/'exclude' => mai.
            _is_thinking = (not _force_non_thinking_tools) and _policy_tools in (
                "native",
                "disable_for_tools",
            )
            config_temperature = None if _is_thinking else temperature
            thinking_config = None
            # Tetto di output effettivo passato a Vertex. Di default coincide con
            # i max_tokens richiesti dal chiamante; per i modelli thinking viene
            # alzato (vedi sotto) per non far erodere l'output dal reasoning.
            _effective_output_tokens = max_tokens
            if _is_thinking:
                try:
                    # ROOT CAUSE hollow completion: su Gemini 2.5 i token di
                    # reasoning sono conteggiati DENTRO max_output_tokens. Se
                    # lasciassimo max_output_tokens=max_tokens, un reasoning lungo
                    # consuma tutto il budget e il modello chiude con
                    # finish_reason=MAX_TOKENS e output VUOTO (hollow completion),
                    # che la fallback chain interpreta come risposta vuota valida.
                    # Fix: budget di thinking DEDICATO e tetto totale alzato a
                    # (output desiderato + thinking), cosi' i max_tokens richiesti
                    # restano interamente disponibili per la risposta utente.
                    # Budget base DB-driven (providers.google.thinking_budget,
                    # regola G), default 8192. Se max_tokens < 256 si disabilita
                    # il thinking (troppo poco spazio anche solo per la risposta).
                    from brain.utils.settings_db import get_int_setting
                    _tb_base = get_int_setting("providers.google.thinking_budget", 8192)
                    if max_tokens >= 256:
                        _tb = max(128, min(_tb_base, max_tokens))
                        thinking_config = types.ThinkingConfig(
                            include_thoughts=True, thinking_budget=_tb,
                        )
                        _effective_output_tokens = max_tokens + _tb
                    else:
                        thinking_config = None
                except Exception:
                    # SDK piu' vecchio senza ThinkingConfig/thinking_budget: fallback
                    # a solo include_thoughts; se anche quello non esiste, nessun thinking.
                    try:
                        thinking_config = types.ThinkingConfig(include_thoughts=True)
                    except Exception:
                        thinking_config = None
            # Anti-narration: al primo turno (nessun tool_result nella history),
            # forza il modello a fare almeno una tool call. Google Gemini usa
            # tool_config con FunctionCallingConfig(mode="ANY").
            tool_config = None
            if google_tools:
                _norm_msgs = [m if isinstance(m, dict) else {} for m in messages]
                if cap is not None:
                    from .adapter_base import resolve_tool_choice
                    _tc = resolve_tool_choice(
                        cap, _norm_msgs, force_override=force_tool_choice
                    )
                    _mode = (_tc or {}).get("function_calling_config", {}).get("mode", "AUTO")
                    tool_config = types.ToolConfig(
                        function_calling_config=types.FunctionCallingConfig(mode=_mode)
                    )
                else:
                    from ._schema_utils import is_first_agent_turn
                    # Senza capability: override esplicito ha priorita', altrimenti
                    # forza solo al primo turno (comportamento storico).
                    if force_tool_choice is True or (
                        force_tool_choice is None and is_first_agent_turn(_norm_msgs)
                    ):
                        tool_config = types.ToolConfig(
                            function_calling_config=types.FunctionCallingConfig(mode="ANY")
                        )

            _cfg_kwargs = dict(
                max_output_tokens=_effective_output_tokens,
                temperature=config_temperature,
                tools=google_tools,
                tool_config=tool_config,
            )
            if thinking_config is not None:
                _cfg_kwargs["thinking_config"] = thinking_config
            config = types.GenerateContentConfig(**_cfg_kwargs)
            if system_text:
                config.system_instruction = system_text

            response = await client.aio.models.generate_content(
                model=model,
                contents=contents,
                config=config,
            )

            # Normalizza risposta al formato Anthropic
            text_content = ""
            thoughts_content = ""  # Reasoning interno del modello (include_thoughts=True).
            stop_reason = "end_turn"
            tool_use_blocks: list[dict] = []
            assistant_content: list[dict] = []

            if response.candidates and response.candidates[0].content and response.candidates[0].content.parts:
                for part in response.candidates[0].content.parts:
                    if part.text:
                        # I "thoughts" (reasoning interno dei modelli 2.5)
                        # arrivano come part.text con part.thought=True.
                        # Vanno separati dalla risposta utente cosi' la UI
                        # puo' mostrarli in un blocco dedicato.
                        if getattr(part, "thought", False):
                            thoughts_content += part.text
                        else:
                            text_content += part.text
                    elif part.function_call:
                        stop_reason = "tool_use"
                        fc = part.function_call
                        # Genera un ID univoco per il tool_use block
                        import uuid
                        tool_id = f"toolu_{uuid.uuid4().hex[:24]}"
                        args = dict(fc.args) if fc.args else {}
                        block = {"id": tool_id, "name": fc.name, "input": args}
                        tool_use_blocks.append(block)
                        assistant_content.append({"type": "tool_use", **block})

            if not tool_use_blocks and text_content:
                assistant_content.append({"type": "text", "text": text_content})

            # finish_reason esplicito (recovery fix Vertex): i modelli thinking
            # possono chiudere con output vuoto (0 testo + 0 tool call). Senza
            # questo, lo stop_reason resterebbe 'end_turn' e la fallback chain
            # tratterebbe il troncamento/malformed come risposta valida vuota.
            # Mappiamo il finish_reason reale a uno stop_reason esplicito cosi'
            # classify/registry possono ritentare in modo informato.
            if not tool_use_blocks and not text_content:
                _fr = None
                if response.candidates:
                    _frv = getattr(response.candidates[0], "finish_reason", None)
                    _fr = getattr(_frv, "name", None) or (str(_frv) if _frv is not None else None)
                _fru = (_fr or "").upper()
                if "MAX_TOKENS" in _fru:
                    stop_reason = "max_tokens"
                    logger.warning(
                        "google_provider: %s finish_reason=MAX_TOKENS con output vuoto "
                        "(thinking-exhaustion: il reasoning ha consumato max_output_tokens=%s). "
                        "stop_reason=max_tokens per fallback informato.", model, max_tokens,
                    )
                elif _fru and _fru not in ("STOP", "FINISH_REASON_UNSPECIFIED", "NONE"):
                    # MALFORMED_FUNCTION_CALL / SAFETY / RECITATION / altro.
                    stop_reason = "content_filter"
                    try:
                        _tn = [t.get("name", "?") for t in (tools or [])][:30]
                    except Exception:
                        _tn = ["<err>"]
                    _um = getattr(response, "usage_metadata", None)
                    logger.warning(
                        "google_provider: %s output vuoto finish_reason=%s | tools=%d %s | "
                        "messages=%d | system_len=%d | tokens in=%s out=%s",
                        model, _fr, len(tools or []), _tn,
                        len(messages) if isinstance(messages, list) else 0,
                        len(system_text or ""),
                        getattr(_um, "prompt_token_count", None) if _um else None,
                        getattr(_um, "candidates_token_count", None) if _um else None,
                    )

            # Usage
            usage_data = {}
            if response.usage_metadata:
                usage_data = {
                    "input_tokens": response.usage_metadata.prompt_token_count or 0,
                    "output_tokens": response.usage_metadata.candidates_token_count or 0,
                }

            metadata = {
                "stop_reason": stop_reason,
                "tool_use_blocks": tool_use_blocks,
                "assistant_content": assistant_content,
                "usage": usage_data,
            }
            if thoughts_content:
                # Espone i thoughts del modello cosi' il consumer (nodes.py /
                # web-ide) possa visualizzarli in un blocco dedicato.
                # Stesso pattern usato da anthropic_provider per extended_thinking.
                metadata["thoughts"] = thoughts_content
                metadata["thinking_content"] = thoughts_content
            return ProviderResult(
                provider=self.name,
                model=model,
                content=text_content,
                metadata=metadata,
            )
        except Exception as e:
            meta = format_error_result(e, self.name, model)
            return ProviderResult(
                provider=self.name, model=model,
                content=f"[Error: {meta['error']}]",
                metadata=meta,
            )

    async def generate_stream(self, model: str, prompt: str, **kwargs: Any) -> AsyncIterator[str]:
        ok, reason = self._is_configured()
        if not ok:
            yield f"[Google provider non configurato: {reason}]"
            return
        try:
            from google.genai import types  # type: ignore[import]
            client = self._get_client()
            async for chunk in await client.aio.models.generate_content_stream(
                model=model,
                contents=prompt,
                config=types.GenerateContentConfig(
                    max_output_tokens=kwargs.get("max_tokens", 4096),
                    temperature=kwargs.get("temperature", 0.7),
                ),
            ):
                if chunk.text:
                    yield chunk.text
        except Exception as e:
            logger.error("Google stream failed: %s", e)
            yield f"[Error: {e}]"

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


def _clean_schema_for_google(schema: dict) -> dict:
    """Rimuovi chiavi non supportate da Google genai e applica compressione (BP6).

    Delega al modulo condiviso _schema_utils.compress_schema che rimuove
    additionalProperties/$schema/default/examples/title, tronca description
    a 200 char e enum a 10 valori. Backward compatible con callers esistenti.
    """
    from ._schema_utils import compress_schema
    return compress_schema(schema)


def _convert_messages_to_google(messages: list[dict]) -> list[Any]:
    """Converte messaggi formato Anthropic (con tool_use/tool_result) in formato Google genai Contents."""
    from google.genai import types  # type: ignore[import]

    # Mappa tool_use_id -> tool_name per risolvere i tool_result
    id_to_name: dict[str, str] = {}
    for msg in messages:
        content = msg.get("content", "")
        if isinstance(content, list):
            for block in content:
                if block.get("type") == "tool_use":
                    id_to_name[block.get("id", "")] = block["name"]

    contents: list[Any] = []
    for msg in messages:
        role = msg.get("role", "user")
        # Google usa "user" e "model" (non "assistant")
        g_role = "model" if role == "assistant" else "user"
        content = msg.get("content", "")

        if isinstance(content, str):
            contents.append(types.Content(
                role=g_role,
                parts=[types.Part.from_text(text=content)],
            ))
        elif isinstance(content, list):
            parts: list[Any] = []
            tool_response_parts: list[Any] = []
            for block in content:
                btype = block.get("type")
                if btype == "text":
                    text_val = block.get("text", "")
                    if text_val:
                        parts.append(types.Part.from_text(text=text_val))
                elif btype == "tool_use":
                    # Blocco tool_use (assistant chiede di chiamare un tool)
                    parts.append(types.Part.from_function_call(
                        name=block["name"],
                        args=block.get("input", {}),
                    ))
                elif btype == "tool_result":
                    # Blocco tool_result — Google vuole il name del tool, non l'id
                    result_content = block.get("content", "")
                    if isinstance(result_content, list):
                        result_content = " ".join(
                            b.get("text", "") for b in result_content if b.get("type") == "text"
                        )
                    tool_use_id = block.get("tool_use_id", "")
                    tool_name = id_to_name.get(tool_use_id, tool_use_id)
                    tool_response_parts.append(types.Part.from_function_response(
                        name=tool_name,
                        response={"result": str(result_content)},
                    ))

            if tool_response_parts:
                # I tool_result vanno come messaggio "user" separato
                contents.append(types.Content(
                    role="user",
                    parts=tool_response_parts,
                ))
            if parts:
                contents.append(types.Content(
                    role=g_role,
                    parts=parts,
                ))
        else:
            contents.append(types.Content(
                role=g_role,
                parts=[types.Part.from_text(text=str(content))],
            ))

    return contents
