"""Implementazione dei nodi del grafo LangGraph per Nexus.

Ogni nodo riceve AgentState e restituisce un dict con i campi aggiornati.
I nodi usano i servizi globali di Nexus (providers, router, embeddings)
invece di dipendenze esterne come LiteLLM.
"""
from __future__ import annotations

import asyncio
import hashlib
import json
import logging
import os
import random
import time
import uuid
from typing import TYPE_CHECKING, Any

from langchain_core.messages import AIMessage, HumanMessage, ToolMessage

from . import (
    meta_steps,
    profile_loader,
    prompt_registry,
    prompt_renderer,
    reflection_config,
    orchestrator_config,
    todo_store,
)
from .reflection_rubric import build_reflection_prompt, parse_reflection_response
from .state import AgentState

if TYPE_CHECKING:
    from brain.embeddings import EmbeddingService
    from brain.grpc_clients.agent_router_client import AgentRouterClient
    from brain.grpc_clients.tool_runner_client import ToolRunnerClient
    from brain.memory.retrieval import InteractionRetriever
    from brain.memory.storage import PostgresLearningStorage as LocalLearningStorage
    from brain.providers import ProviderRegistry
    from brain.router import SemanticRouter

logger = logging.getLogger(__name__)

# Cap iterazioni agent loop (richiesta executor -> tool_dispatch -> executor ...).
# Default conservativo: il valore reale per ogni run e' calcolato adattivamente da
# `compute_iteration_budget()` sotto, sulla base della complessita' del prompt
# e dei settings DB (agent.iteration_budget.*). Questo MAX_AGENT_ITERATIONS resta
# come fallback se il DB non e' raggiungibile o se il budget non e' stato
# popolato in state["iteration_budget"] dal router_node.
#
# Mig 0181 (adaptive_agent_budget): i task semplici prendono ~base (60), i task
# complessi multi-step (fullstack scaffolding, refactor end-to-end) arrivano a
# 200-300 senza modificare il codice.
MAX_AGENT_ITERATIONS = 60

# ── Cache adaptive budget settings (TTL 60s) ─────────────────────────────────
# Evita N query al DB per ogni run mantenendo i valori freschi al cambio runtime.
_ADAPTIVE_BUDGET_CACHE: dict[str, Any] = {"loaded_at": 0.0, "config": None}
_ADAPTIVE_BUDGET_TTL_SEC = 60.0
_ADAPTIVE_BUDGET_DEFAULTS = {
    "iteration_budget_base": 60,
    "iteration_budget_per_complexity_point": 4,
    "iteration_budget_max": 300,
    "complexity_step_marker_points": 5,
    "complexity_file_path_points": 2,
    "complexity_keyword_weights": {
        "create": 3, "write_file": 2, "install": 2, "build": 2, "systemctl": 2,
        "docker": 2, "pnpm": 2, "npm": 1, "deploy": 3, "migrate": 3,
        "refactor": 4, "fullstack": 10, "end-to-end": 8, "backend": 2,
        "frontend": 2, "database": 2, "crea": 3, "installa": 2, "esegui": 2,
        "avvia": 2, "configura": 2,
    },
    "weak_model_multiplier": 1.5,
}

import re as _re_budget

_WEAK_MODELS_HINT = ("mini", "nano", "haiku", "lite", "small", "flash-lite")
_STEP_MARKER_RE = _re_budget.compile(r"\b(?:\d+\.|step\s+\d+|task\s+\d+|phase\s+\d+|fase\s+\d+|passo\s+\d+)\b", _re_budget.IGNORECASE)
_FILE_PATH_RE = _re_budget.compile(r"(?:/[a-zA-Z0-9_.-]+){2,}|[a-zA-Z0-9_-]+\.(?:js|ts|tsx|jsx|py|rs|json|yml|yaml|sql|md|env|html|css|toml|sh)")


def _load_adaptive_budget_config() -> dict[str, Any]:
    """Carica i settings agent.iteration_budget.* dal DB con cache 60s.

    Ritorna dict con tutte le chiavi richieste (con fallback ai defaults).
    Mai solleva: se il DB e' down, ritorna i defaults hardcoded per garantire
    che l'agente continui a funzionare anche in degraded mode.
    """
    now = time.time()
    if _ADAPTIVE_BUDGET_CACHE["config"] is not None and (now - _ADAPTIVE_BUDGET_CACHE["loaded_at"]) < _ADAPTIVE_BUDGET_TTL_SEC:
        return _ADAPTIVE_BUDGET_CACHE["config"]

    config = dict(_ADAPTIVE_BUDGET_DEFAULTS)
    try:
        import os as _os
        import psycopg2
        db_url = _os.environ.get("DATABASE_URL") or "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable"
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT key, value FROM settings WHERE key LIKE 'agent.iteration_budget.%%' OR key LIKE 'agent.complexity.%%'"
                )
                for key, value in cur.fetchall():
                    if key == "agent.iteration_budget.base":
                        config["iteration_budget_base"] = int(value)
                    elif key == "agent.iteration_budget.per_complexity_point":
                        config["iteration_budget_per_complexity_point"] = int(value)
                    elif key == "agent.iteration_budget.max":
                        config["iteration_budget_max"] = int(value)
                    elif key == "agent.complexity.step_marker_points":
                        config["complexity_step_marker_points"] = int(value)
                    elif key == "agent.complexity.file_path_points":
                        config["complexity_file_path_points"] = int(value)
                    elif key == "agent.complexity.keyword_weights":
                        try:
                            config["complexity_keyword_weights"] = json.loads(value)
                        except Exception:
                            pass
                    elif key == "agent.complexity.weak_model_multiplier":
                        try:
                            config["weak_model_multiplier"] = float(value)
                        except Exception:
                            pass
    except Exception as exc:
        logger.warning("adaptive_budget: load DB fallito, uso defaults (%s)", exc)

    _ADAPTIVE_BUDGET_CACHE["config"] = config
    _ADAPTIVE_BUDGET_CACHE["loaded_at"] = now
    return config


# ── Cache G1 nudge cap (TTL 60s) ────────────────────────────────────────────
# Numero massimo di re-execution G1 ("risposta descrittiva su action request")
# per singolo run prima di forzare chiusura con messaggio assistant esplicito.
# Letto dal DB via settings_db.get_int_setting con default 3.
_G1_NUDGE_CACHE: dict[str, Any] = {"loaded_at": 0.0, "max_nudges": None}
_G1_NUDGE_TTL_SEC = 60.0
_G1_NUDGE_DEFAULT_MAX = 3


def _load_g1_max_nudges() -> int:
    """Legge agent.g1_max_nudges dal DB con cache 60s.

    Ritorna il default sicuro (3) se il DB e' irraggiungibile o la chiave
    non esiste: la funzione `get_int_setting` non solleva mai.
    """
    now = time.time()
    cached = _G1_NUDGE_CACHE["max_nudges"]
    if cached is not None and (now - _G1_NUDGE_CACHE["loaded_at"]) < _G1_NUDGE_TTL_SEC:
        return int(cached)
    try:
        from brain.utils.settings_db import get_int_setting
        value = int(get_int_setting("agent.g1_max_nudges", _G1_NUDGE_DEFAULT_MAX))
        if value < 1:
            value = _G1_NUDGE_DEFAULT_MAX
    except Exception as exc:
        logger.warning("g1_max_nudges: load DB fallito, uso default %d (%s)", _G1_NUDGE_DEFAULT_MAX, exc)
        value = _G1_NUDGE_DEFAULT_MAX
    _G1_NUDGE_CACHE["max_nudges"] = value
    _G1_NUDGE_CACHE["loaded_at"] = now
    return value


# ── Loop-detection semantica: tool di sola esplorazione ─────────────────────
# Tool che leggono/ispezionano allegati e file senza produrre codice o
# side-effect. Quando il modello ne incatena troppi di fila (variando
# entry/offset, cosi' la loop-detection per signature identica non scatta) e'
# bloccato in esplorazione e va spinto a scrivere. I nomi sono quelli usati in
# _estimate_tool_result_size_bytes (read_file e' il tool di lettura sorgente).
_EXPLORATION_ONLY_TOOLS: frozenset[str] = frozenset({
    "nexus_list_archive_entries", "nexus_read_archive_entry",
    "nexus_inspect_attachment", "nexus_extract_figma_structure",
    "nexus_list_attachments", "nexus_read_attachment",
    "nexus_extract_docx_text", "nexus_extract_xlsx_data",
    "nexus_extract_pdf_text", "nexus_describe_image_attachment",
    "read_file",
})

# ── Cache soglia loop esplorativo (TTL 60s) ─────────────────────────────────
# Numero di chiamate esplorative consecutive oltre il quale iniettiamo un
# nudge forte verso la scrittura. A 2x la soglia abortiamo. Letta dal DB via
# settings_db.get_int_setting con default 6 (regola G: niente hardcode).
_EXPLORATION_LOOP_CACHE: dict[str, Any] = {"loaded_at": 0.0, "threshold": None}
_EXPLORATION_LOOP_TTL_SEC = 60.0
_EXPLORATION_LOOP_DEFAULT = 6


def _load_exploration_loop_threshold() -> int:
    """Legge agent.exploration_loop_threshold dal DB con cache 60s.

    Ritorna il default sicuro (6) se il DB e' irraggiungibile o la chiave non
    esiste: get_int_setting non solleva mai.
    """
    now = time.time()
    cached = _EXPLORATION_LOOP_CACHE["threshold"]
    if cached is not None and (now - _EXPLORATION_LOOP_CACHE["loaded_at"]) < _EXPLORATION_LOOP_TTL_SEC:
        return int(cached)
    try:
        from brain.utils.settings_db import get_int_setting
        value = int(get_int_setting("agent.exploration_loop_threshold", _EXPLORATION_LOOP_DEFAULT))
        if value < 1:
            value = _EXPLORATION_LOOP_DEFAULT
    except Exception as exc:
        logger.warning(
            "exploration_loop_threshold: load DB fallito, uso default %d (%s)",
            _EXPLORATION_LOOP_DEFAULT, exc,
        )
        value = _EXPLORATION_LOOP_DEFAULT
    _EXPLORATION_LOOP_CACHE["threshold"] = value
    _EXPLORATION_LOOP_CACHE["loaded_at"] = now
    return value


# ── Cache reminder lingua resiliente (TTL 60s) ──────────────────────────────
# Bug #88: a contesto saturo (>400K token) i modelli small con forte recency
# bias ignorano la direttiva di lingua presente solo in testa al system prompt
# e rispondono in cinese, allucinando l'identita'. Iniettiamo SEMPRE un reminder
# di lingua in coda al system_text e in coda all'ultimo HumanMessage (recency),
# coprendo cosi' anche i profili custom e gli 82 template senza direttiva.
# I default valgono SOLO se il DB e' down (regola G: niente hardcode sparso).
_LANG_REMINDER_MARKER = "[[NEXUS_LANG_REMINDER]]"
_LANG_REMINDER_CACHE: dict[str, Any] = {
    "loaded_at": 0.0,
    "enabled": None,
    "text": None,
}
_LANG_REMINDER_TTL_SEC = 60.0
_LANG_REMINDER_DEFAULT_ENABLED = True
_LANG_REMINDER_DEFAULT_TEXT = (
    "Rispondi SEMPRE e SOLO in italiano. Mai cinese, giapponese o altre "
    "lingue, qualunque sia la lingua del contesto o degli allegati."
)


def _load_language_reminder() -> tuple[bool, str]:
    """Legge agent.language_reminder_enabled / _text dal DB con cache 60s.

    Ritorna (enabled, text). I default sicuri (True, testo italiano) valgono
    SOLO se il DB e' irraggiungibile o la chiave non esiste: get_bool_setting /
    get_setting non sollevano mai. Quando enabled e' False l'iniezione del
    reminder non avviene affatto (vedi _inject_language_reminder).
    """
    now = time.time()
    cached_enabled = _LANG_REMINDER_CACHE["enabled"]
    if cached_enabled is not None and (
        now - _LANG_REMINDER_CACHE["loaded_at"]
    ) < _LANG_REMINDER_TTL_SEC:
        return bool(cached_enabled), str(_LANG_REMINDER_CACHE["text"])
    enabled = _LANG_REMINDER_DEFAULT_ENABLED
    text = _LANG_REMINDER_DEFAULT_TEXT
    try:
        from brain.utils.settings_db import get_bool_setting, get_setting
        enabled = get_bool_setting(
            "agent.language_reminder_enabled", _LANG_REMINDER_DEFAULT_ENABLED
        )
        text = get_setting(
            "agent.language_reminder_text", _LANG_REMINDER_DEFAULT_TEXT
        ).strip() or _LANG_REMINDER_DEFAULT_TEXT
    except Exception as exc:
        logger.warning(
            "language_reminder: load DB fallito, uso default (enabled=%s) (%s)",
            _LANG_REMINDER_DEFAULT_ENABLED, exc,
        )
        enabled = _LANG_REMINDER_DEFAULT_ENABLED
        text = _LANG_REMINDER_DEFAULT_TEXT
    _LANG_REMINDER_CACHE["enabled"] = enabled
    _LANG_REMINDER_CACHE["text"] = text
    _LANG_REMINDER_CACHE["loaded_at"] = now
    return enabled, text


def _inject_language_reminder(
    messages: list[Any],
    system_text: str,
    enabled: bool,
    reminder_text: str,
) -> tuple[list[Any], str]:
    """Inietta il reminder di lingua nel system_text e nell'ultimo HumanMessage.

    Funzione pura e idempotente (testabile senza far girare executor_node):

      1. GARANZIA NEL SYSTEM: appende il reminder in coda a system_text con
         marcatore univoco _LANG_REMINDER_MARKER. Copre profili/template senza
         direttiva di lingua.
      2. RECENCY NEI MESSAGGI: appende il reminder al content dell'ULTIMO
         HumanMessage con content stringa (recency bias dei modelli small).
         NON aggiunge un nuovo messaggio (l'alternanza user/assistant richiesta
         da Anthropic resterebbe rotta). Crea una COPIA del messaggio, non muta
         l'oggetto originale dello stato condiviso. Se l'ultimo HumanMessage ha
         content non-stringa (lista di blocchi) il punto 2 viene saltato: il
         punto 1 garantisce comunque la copertura.

    Ritorna (messages, system_text) eventualmente modificati. Se enabled e'
    False ritorna gli input invariati. Idempotente: il marcatore e il testo
    gia' presenti non vengono riappesi.
    """
    if not enabled or not reminder_text:
        return messages, system_text

    # Punto 1: garanzia nel system_text (idempotente via marcatore).
    base_system = system_text or ""
    if _LANG_REMINDER_MARKER not in base_system:
        new_system = (
            f"{base_system}\n\n{_LANG_REMINDER_MARKER}\n"
            f"### LINGUA RISPOSTA OBBLIGATORIA ###\n{reminder_text}"
        )
    else:
        new_system = system_text

    # Punto 2: recency sull'ultimo HumanMessage con content stringa.
    new_messages = messages
    for idx in range(len(messages) - 1, -1, -1):
        msg = messages[idx]
        if not isinstance(msg, HumanMessage):
            continue
        content = getattr(msg, "content", None)
        if not isinstance(content, str):
            # Ultimo HumanMessage ha content non-stringa (lista blocchi):
            # salta il punto 2, il punto 1 copre comunque.
            break
        if reminder_text in content:
            # Gia' presente: idempotenza, niente riappensione.
            break
        new_content = f"{content}\n\n{reminder_text}"
        new_msg = msg.model_copy(update={"content": new_content})
        new_messages = list(messages)
        new_messages[idx] = new_msg
        break

    return new_messages, new_system


def estimate_prompt_complexity(prompt: str, config: dict[str, Any] | None = None) -> int:
    """Stima la complessita' del task come score 0-100.

    Punteggio = somma(keyword_match * peso) + n_step_markers * step_points
              + n_file_paths * file_path_points, capped a 100.

    Deterministico: stesso prompt -> stesso score. Idempotente: nessun side effect.
    """
    if not prompt:
        return 0
    if config is None:
        config = _load_adaptive_budget_config()
    text = prompt.lower()
    score = 0
    # Keyword weighted match (substring).
    for keyword, weight in config["complexity_keyword_weights"].items():
        if keyword in text:
            score += int(weight)
    # Step markers (1., 2., step N, fase N, ...).
    n_steps = len(_STEP_MARKER_RE.findall(prompt))
    score += n_steps * int(config["complexity_step_marker_points"])
    # File path / extension markers.
    n_paths = len(_FILE_PATH_RE.findall(prompt))
    score += n_paths * int(config["complexity_file_path_points"])
    return min(score, 100)


def compute_iteration_budget(prompt: str, model: str | None = None) -> tuple[int, int]:
    """Calcola il budget di iterazioni per un run agente.

    Ritorna (iter_budget, complexity_score). Il budget e':
        base + per_complexity_point * complexity_score, scalato per weak model,
        capped a max.

    Esempi (config default 60/4/300):
        prompt semplice (score=0)   -> 60 iter
        prompt medio    (score=20)  -> 140 iter
        prompt complesso (score=50) -> 260 iter
        prompt fullstack (score>=60)-> 300 iter (cap)
    """
    config = _load_adaptive_budget_config()
    score = estimate_prompt_complexity(prompt, config)
    base = int(config["iteration_budget_base"])
    per_pt = int(config["iteration_budget_per_complexity_point"])
    budget = base + per_pt * score
    # Modelli weak (mini/nano/haiku/lite) tendono a fare G1-nudge senza chiamare
    # tool: gli serve piu' budget per arrivare al risultato.
    if model and any(hint in model.lower() for hint in _WEAK_MODELS_HINT):
        budget = int(budget * float(config["weak_model_multiplier"]))
    return min(budget, int(config["iteration_budget_max"])), score

# ── Nexus thinking: stream pensieri agente verso UI ──────────────────────
# Espone un'opzione "mostra ragionamento Nexus" leggibile dal flag DB
# `chat_show_nexus_thinking` (settings). Quando attivo (default true), i
# nodi router/executor possono accodare righe in `nexus_thinking` (list[str])
# dentro il delta che il graph ritorna; il bridge SSE in brain/grpc_server
# le converte in eventi `thinking_delta`.
_NEXUS_THINKING_CACHE: dict[str, Any] = {"loaded_at": 0.0, "enabled": True}
_NEXUS_THINKING_TTL_SEC = 60.0


def _nexus_thinking_enabled() -> bool:
    """True se il flag DB `chat_show_nexus_thinking` e' attivo.

    Cache TTL 60s per evitare query al DB ad ogni emissione. Se il DB e'
    irraggiungibile o il flag e' assente, fallback al default True (UX
    visibile per scoperta della feature; l'utente puo' disattivare).
    """
    now = time.time()
    cached_at = float(_NEXUS_THINKING_CACHE.get("loaded_at") or 0.0)
    if now - cached_at < _NEXUS_THINKING_TTL_SEC:
        return bool(_NEXUS_THINKING_CACHE.get("enabled", True))
    enabled = True
    try:
        import os as _os
        import psycopg2  # type: ignore[import-untyped]
        dburl = _os.environ.get("DATABASE_URL", "")
        if dburl:
            conn = psycopg2.connect(dburl)
            try:
                with conn.cursor() as cur:
                    cur.execute(
                        "SELECT value FROM settings WHERE key = %s",
                        ("chat_show_nexus_thinking",),
                    )
                    row = cur.fetchone()
                    if row and row[0] is not None:
                        raw = str(row[0]).strip().lower().strip('"')
                        enabled = raw not in ("false", "0", "off", "no")
            finally:
                conn.close()
    except Exception as exc:
        logger.debug("_nexus_thinking_enabled: lettura DB fallita: %s", exc)
    _NEXUS_THINKING_CACHE["loaded_at"] = now
    _NEXUS_THINKING_CACHE["enabled"] = enabled
    return enabled


# Diagnostica FIX 3 (streaming live thinking): contatore one-shot per loggare
# WARNING la prima volta che get_stream_writer() non e' disponibile, senza
# spammare i log a ogni riga di thinking.
_STREAM_WRITER_DIAG: dict[str, bool] = {"warned_none": False, "warned_exc": False}


def _stream_thinking_live(line: str) -> None:
    """Pusha immediatamente un evento `nexus_thinking` sul custom stream
    di LangGraph (visibile a stream_mode="custom" nell endpoint SSE).

    LangGraph 0.2+ espone `get_stream_writer()`: se la chiamata e'
    eseguita dentro un nodo del grafo, il writer permette di emettere
    eventi custom che il consumer riceve immediatamente, senza aspettare
    il return del nodo. Cosi' la UI mostra il thinking in tempo reale
    anche se il nodo dura 30-60 secondi.

    Best-effort: se non siamo dentro un grafo (test, codice riusato) o
    la versione di LangGraph installata non espone l API, no-op.
    """
    if not line:
        return
    try:
        from langgraph.config import get_stream_writer  # type: ignore[import-untyped]
        writer = get_stream_writer()
        if writer is None:
            # Diagnostica FIX 3: il writer e' None solo se il nodo gira fuori
            # da un astream(stream_mode che include "custom") — es. ainvoke()
            # o subagent dispatch. Logghiamo WARNING la PRIMA volta per non
            # restare ciechi, poi degradiamo a debug per non spammare.
            if not _STREAM_WRITER_DIAG["warned_none"]:
                _STREAM_WRITER_DIAG["warned_none"] = True
                logger.warning(
                    "_stream_thinking_live: get_stream_writer() ha ritornato None — "
                    "il thinking live NON parte. Il nodo gira fuori da astream(stream_mode "
                    "che include 'custom')? (questo WARNING e' emesso una sola volta)"
                )
            else:
                logger.debug("_stream_thinking_live: writer None (gia' diagnosticato)")
            return
        writer({"kind": "nexus_thinking", "text": str(line).strip()})
    except Exception as exc:
        # Niente raise: il thinking live e' best-effort, non deve
        # rompere l esecuzione del nodo.
        if not _STREAM_WRITER_DIAG["warned_exc"]:
            _STREAM_WRITER_DIAG["warned_exc"] = True
            logger.warning(
                "_stream_thinking_live: writer non disponibile (%s) — thinking live "
                "non emesso. (questo WARNING e' emesso una sola volta)", exc
            )
        else:
            logger.debug("_stream_thinking_live: writer non disponibile: %s", exc)


def _emit_thinking(updates: dict[str, Any], *lines: str) -> None:
    """Accoda `lines` non vuote nel campo `nexus_thinking` di `updates`
    E le pusha in tempo reale via stream_writer (fix streaming live).

    No-op se il flag DB e' disabilitato o se nessuna riga e' significativa.
    Crea il campo come `list[str]` se assente, altrimenti append in-place.
    L append e' mantenuto per backward-compat (final state) ed e' come il
    consumer SSE riceve gia' oggi gli eventi di fine nodo.
    """
    if not lines:
        return
    cleaned = [str(x).strip() for x in lines if x is not None and str(x).strip()]
    if not cleaned:
        return
    if not _nexus_thinking_enabled():
        return
    # 1) Stream live (LangGraph custom events): l utente vede subito le
    #    righe di thinking, senza attendere il return del nodo.
    for ln in cleaned:
        _stream_thinking_live(ln)
    # 2) Backward-compat: accoda in updates per il delta finale del nodo.
    existing = updates.get("nexus_thinking")
    if isinstance(existing, list):
        existing.extend(cleaned)
    else:
        updates["nexus_thinking"] = cleaned


def _describe_tool_call(name: str, args: dict[str, Any] | None) -> str:
    """Traduce una tool call in una frase italiana leggibile per il thinking.

    Deriva contesto utile dagli `args` quando disponibile (path del file,
    label della porta, comando, ecc.). Fallback generico se il tool non e'
    mappato. Mai solleva: input malformati ricadono sul fallback.
    """
    nm = (name or "sconosciuto").strip()
    a = args if isinstance(args, dict) else {}

    def _s(key: str, default: str = "") -> str:
        try:
            v = a.get(key)
            return str(v).strip() if v is not None else default
        except Exception:
            return default

    if nm in ("write_file", "create_file"):
        p = _s("path") or _s("file_path") or _s("filename")
        return f"Scrivo il file {p}" if p else "Scrivo un file"
    if nm in ("edit_file", "apply_patch", "str_replace"):
        p = _s("path") or _s("file_path")
        return f"Modifico il file {p}" if p else "Modifico un file"
    if nm in ("read_file", "nexus_read_attachment"):
        p = _s("path") or _s("file_path") or _s("attachment_id")
        return f"Leggo {p}" if p else "Leggo un file"
    if nm in ("list_dir", "list_files", "nexus_list_attachments"):
        p = _s("path") or _s("dir")
        return f"Elenco il contenuto di {p}" if p else "Elenco i file"
    if nm == "request_port":
        lbl = _s("label")
        return f"Richiedo una porta per {lbl}" if lbl else "Richiedo una porta"
    if nm in ("run_command", "shell_exec", "execute_command", "bash"):
        cmd = _s("command") or _s("cmd")
        return f"Eseguo: {cmd[:80]}" if cmd else "Eseguo un comando shell"
    if nm == "nexus_extract_figma_structure":
        return "Estraggo la specifica dal file Figma allegato"
    if nm == "nexus_extract_pdf_text":
        return "Estraggo il testo dal PDF allegato"
    if nm in ("nexus_extract_docx_text", "nexus_extract_xlsx_data"):
        return "Estraggo i dati dal documento allegato"
    if nm == "nexus_inspect_attachment":
        return "Ispeziono l'allegato per capire come elaborarlo"
    if nm == "nexus_describe_image_attachment":
        return "Analizzo l'immagine allegata con il modello vision"
    if nm in ("nexus_list_archive_entries", "nexus_read_archive_entry"):
        return "Esploro il contenuto dell'archivio allegato"
    if nm in ("web_search", "search"):
        q = _s("query") or _s("q")
        return f"Cerco sul web: {q[:80]}" if q else "Cerco sul web"
    return f"Chiamo tool: {nm}"


# ── G1 Python: rilevamento richieste d'azione ─────────────────────────────
_ACTION_PATTERNS: tuple[str, ...] = (
    # Italiano — imperativo / infinito / futuro
    "avvia", "avviare", "lancia", "lanciare",
    "esegui", "eseguire",
    "builda", "buildare",
    "crea ", "creare", "crea il", "crea la",
    "installa", "installare",
    "configura", "configurare",
    "deploya", "deployare",
    "compila", "compilare",
    "fai partire", "metti in piedi", "porta in su", "metti online",
    "avvia i servizi", "avvia il backend", "avvia il frontend", "avvia il server",
    "scaffolda", "inizializza il progetto", "crea il progetto",
    "scrivi i file", "genera il progetto",
    # Inglese — imperativo / common forms
    "start ", "launch ", " run ", "run the", " build", "build the",
    " create ", "create the", "install ", "setup ", "set up ",
    "configure ", "deploy ", "compile ", "scaffold ",
    # Tecnologie specifiche
    "docker", "docker-compose", "docker compose",
    "npm install", "npm run", "pnpm install", "pnpm run",
    "cargo build", "cargo run", "dotnet run", "dotnet build",
    "pip install", "pip3 install", "apt install", "apt-get install",
    "systemctl start", "service start", "make ",
    # Creazione struttura progetto
    "crea la struttura", "crea le directory", "crea i file",
    "structure", "scaffolding",
)


def _detect_action_request(text: str) -> bool:
    """G1 Python: True se il testo contiene una richiesta d'azione concreta.

    Speculare a crates/mcp-core/src/agent_types.rs::detect_action_request.
    Usato per forzare nudge quando l'agente risponde con 0 tool call.
    """
    if not text or not text.strip():
        return False
    lower = text.lower()
    return any(p in lower for p in _ACTION_PATTERNS)


def _has_tool_calls_in_history(messages: list) -> bool:
    """Controlla se nella history ci sono già stati tool call effettivi (AIMessage con tool_use)."""
    for m in messages:
        if not isinstance(m, AIMessage):
            continue
        extra = getattr(m, "additional_kwargs", {}) or {}
        blocks = extra.get("anthropic_content") or []
        if isinstance(blocks, list) and any(
            isinstance(b, dict) and b.get("type") == "tool_use" for b in blocks
        ):
            return True
    return False


# Pattern testuali che indicano errore dentro un tool_result. Match case-insensitive.
# Non e' esaustivo, copre i casi piu' frequenti (npm/cargo/python/shell/network).
_TOOL_ERROR_HINTS = (
    "error:", "errore:", "[error", "exit code: 1", "exit code 1",
    "command failed", "comando fallito", "traceback", "exception:",
    "fatal:", "syntax error", "not found", "non trovato",
    "cannot find module", "module not found", "permission denied",
    "connection refused", "timed out", "timeout", "404 not found",
    "500 internal", "econnrefused", "enoent", "enotfound", "eperm",
    "no such file", "is_error", "[errno",
)


def _detect_repeated_failed_command(messages: list, lookback: int = 12) -> tuple[str | None, int]:
    """Rileva ripetizione dello STESSO comando shell con errore.

    Scansiona gli ultimi `lookback` messaggi (in ordine cronologico inverso) e:
    1. Trova AIMessage con tool_use=run_command|run_service|run_in_terminal
    2. Per ognuno, controlla il ToolMessage SUCCESSIVO per status d'errore
    3. Conta le occorrenze della STESSA signature `command|working_dir`
    4. Ritorna (signature, count) della piu' frequente; None se nessuna ripetizione

    Usato dall'executor_node per iniettare nudge "stesso comando fallito N volte,
    cambia strategia". Risolve il bug del loop npm/shadcn osservato 30/05/2026.
    """
    if not messages:
        return (None, 0)
    failed_signatures: dict[str, int] = {}
    last_signature: str | None = None
    # Iterazione lineare (non reversed) cosi' associo correttamente
    # AIMessage(tool_use) -> ToolMessage(result) successivo.
    recent = messages[-lookback:] if len(messages) > lookback else messages
    for idx, m in enumerate(recent):
        if not isinstance(m, AIMessage):
            continue
        extra = getattr(m, "additional_kwargs", {}) or {}
        blocks = extra.get("anthropic_content") or []
        if not isinstance(blocks, list):
            continue
        for b in blocks:
            if not isinstance(b, dict) or b.get("type") != "tool_use":
                continue
            name = b.get("name", "")
            if name not in ("run_command", "run_service", "run_in_terminal"):
                continue
            inp = b.get("input", {}) or {}
            cmd = str(inp.get("command", "")).strip()
            wd = str(inp.get("working_dir", "")).strip()
            if not cmd:
                continue
            signature = f"{cmd}|{wd}"
            # Cerca il prossimo ToolMessage (entro 3 step) e valuta errore
            next_is_error = False
            for j in range(idx + 1, min(idx + 4, len(recent))):
                nm = recent[j]
                if isinstance(nm, ToolMessage):
                    if getattr(nm, "status", "") == "error":
                        next_is_error = True
                    else:
                        c = getattr(nm, "content", "")
                        if isinstance(c, list):
                            for cc in c:
                                if isinstance(cc, dict):
                                    if cc.get("is_error"):
                                        next_is_error = True
                                        break
                                    txt = str(cc.get("text", "") or cc.get("content", ""))
                                    if any(h in txt.lower() for h in _TOOL_ERROR_HINTS):
                                        next_is_error = True
                                        break
                        else:
                            if any(h in str(c).lower() for h in _TOOL_ERROR_HINTS):
                                next_is_error = True
                    break
            if next_is_error:
                failed_signatures[signature] = failed_signatures.get(signature, 0) + 1
                last_signature = signature
    if not failed_signatures:
        return (None, 0)
    # Ritorna la signature piu' frequente (preferisce l'ultima in caso di parita').
    top = max(failed_signatures.items(), key=lambda kv: (kv[1], kv[0] == last_signature))
    return (top[0].split("|", 1)[0], top[1])


def _detect_recent_tool_error(messages: list, lookback: int = 4) -> bool:
    """True se uno degli ultimi `lookback` ToolMessage indica errore.

    Heuristica: scansiona gli ultimi N messaggi (in ordine inverso) cercando
    ToolMessage e ispezionando il content (str o list di blocchi). Match su
    `is_error=True` esplicito oppure su pattern testuali in `_TOOL_ERROR_HINTS`.

    Usato dal G1 cap per evitare di contare come "descrittivo" un turno in
    cui il modello sta reagendo a tool failure (es. build npm fallita).
    """
    if not messages:
        return False
    checked = 0
    for m in reversed(messages):
        if checked >= lookback:
            break
        if not isinstance(m, ToolMessage):
            continue
        checked += 1
        if getattr(m, "status", "") == "error":
            return True
        content = getattr(m, "content", "")
        if isinstance(content, list):
            for c in content:
                if isinstance(c, dict):
                    if c.get("is_error"):
                        return True
                    txt = str(c.get("text", "") or c.get("content", ""))
                    if any(hint in txt.lower() for hint in _TOOL_ERROR_HINTS):
                        return True
        else:
            if any(hint in str(content).lower() for hint in _TOOL_ERROR_HINTS):
                return True
    return False


# ── Override deterministico: scaffolding applicativo ───────────────────────
# Famiglia di intent "crea un'applicazione / fai una app per X / implementa
# l'app dal file". L'embedding classifier (e l'adaptive LLM quando spento)
# possono sbagliare questa classe e mandarla su `code_read` per vicinanza al
# token "file" ("crea l'app descritta NEL FILE allegato"). Questo override
# replica strutturalmente la regola gia' presente nel prompt dell'agentic
# classifier (brain/router/agentic_classifier.py) ma in modo deterministico,
# robusto anche con embedding/adaptive OFF. Vince sempre il verbo di creazione.
#
# Verbi di creazione (italiano + inglese), con varianti pronominali.
_SCAFFOLD_VERBS: tuple[str, ...] = (
    "crea", "creami", "fai", "fammi", "costruisci", "costruiscimi",
    "realizza", "sviluppa", "implementa", "genera",
    "build", "create", "make", "develop", "scaffold", "generate",
)
# Oggetti applicativi: il target da scaffoldare.
_SCAFFOLD_OBJECTS: tuple[str, ...] = (
    "app", "application", "applicazione", "applicativo",
    "sistema", "system", "sito", "site", "web app", "webapp",
    "piattaforma", "platform", "servizio", "service",
    "gestionale", "software", "programma", "progetto", "project",
    "e-commerce", "ecommerce", "dashboard", "portale", "portal", "booking",
)

# Verbo seguito (anche non immediatamente, entro la frase) da un oggetto
# applicativo. Gestisce articoli/apostrofi ("un'app", "l'applicazione",
# "una app") perche' l'oggetto e' cercato come parola intera in una finestra
# successiva al verbo. La regex e' costruita una sola volta a import-time.
_SCAFFOLD_VERB_RE = _re_budget.compile(
    r"\b(?:" + "|".join(_re_budget.escape(v) for v in _SCAFFOLD_VERBS) + r")\b",
    _re_budget.IGNORECASE,
)
_SCAFFOLD_OBJ_RE = _re_budget.compile(
    r"\b(?:" + "|".join(_re_budget.escape(o) for o in _SCAFFOLD_OBJECTS) + r")\b",
    _re_budget.IGNORECASE,
)


def _detect_scaffolding_intent(text: str) -> bool:
    """True quando il testo chiede di creare un'applicazione/progetto.

    Matcha la famiglia VERBO_di_creazione ... OGGETTO_applicativo nella stessa
    frase (oggetto cercato in una finestra che segue il primo verbo). Robusto
    con articoli e apostrofi ("crea un'app", "fai una app per X"). La presenza
    di "nel file allegato" NON declassa il match: vince il verbo di creazione.
    """
    if not text or not text.strip():
        return False
    # Normalizza apostrofi tipografici per uniformare i confini di parola.
    normalized = text.replace("’", "'")
    verb_match = _SCAFFOLD_VERB_RE.search(normalized)
    if verb_match is None:
        return False
    # Cerca un oggetto applicativo a partire dalla posizione del verbo.
    return _SCAFFOLD_OBJ_RE.search(normalized, verb_match.end()) is not None


# ── Lookup prezzi modelli da ai_price_catalog ──────────────────────────────
_PRICE_CACHE: dict[str, tuple[float, float]] = {}
_PRICE_CACHE_TS: dict[str, float] = {}
_PRICE_TTL_S = 300.0


def _lookup_price(provider: str, model: str) -> tuple[float, float]:
    """Ritorna (input_cost_per_mtok, output_cost_per_mtok) da ai_price_catalog.
    Cache 5min. Ritorna (0,0) se modello non trovato (niente errore bloccante)."""
    import os
    key = f"{provider}|{model}"
    now = time.monotonic()
    if key in _PRICE_CACHE and (now - _PRICE_CACHE_TS.get(key, 0)) < _PRICE_TTL_S:
        return _PRICE_CACHE[key]
    try:
        import psycopg2
        db_url = os.environ.get(
            "DATABASE_URL",
            "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable",
        )
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT input_cost_per_million_tokens, output_cost_per_million_tokens "
                    "FROM ai_price_catalog "
                    "WHERE provider = %s AND model = %s AND is_enabled = TRUE "
                    "ORDER BY effective_from DESC LIMIT 1",
                    (provider, model),
                )
                row = cur.fetchone()
        if row:
            result = (float(row[0]), float(row[1]))
        else:
            result = (0.0, 0.0)
    except Exception as e:
        logger.warning("_lookup_price(%s/%s) fallito: %s", provider, model, e)
        result = (0.0, 0.0)
    _PRICE_CACHE[key] = result
    _PRICE_CACHE_TS[key] = now
    return result

# ── Limiti output tool per gestione contesto ────────────────────────────────
# Tronca risultati tool singoli troppo grandi (20% testa + 80% coda)
MAX_TOOL_RESULT_CHARS = 6000
# Budget totale contesto: oltre questo soglia i tool result vecchi vengono compressi
MAX_CONTEXT_CHARS = 400_000


def _smart_truncate(text: str, max_chars: int = MAX_TOOL_RESULT_CHARS) -> str:
    """Tronca preservando testa (20%) e coda (80%) per mantenere errori finali."""
    if len(text) <= max_chars:
        return text
    head_size = max_chars // 5
    tail_size = max_chars - head_size - 80
    return (
        text[:head_size]
        + f"\n\n[... TRONCATO: {len(text) - head_size - tail_size} caratteri omessi ...]\n\n"
        + text[-tail_size:]
    )


def _smart_truncate_lossless(
    text: str,
    max_chars: int = MAX_TOOL_RESULT_CHARS,
    *,
    source_kind: str = "tool_result",
    metadata: dict[str, Any] | None = None,
) -> str:
    """Variante LOSSLESS di _smart_truncate: prima di tagliare offloada in RAG.

    Se il testo supera `max_chars`, il contenuto COMPLETO viene indicizzato in
    Qdrant (collection tool_results_chunks) via context_offload, poi nel prompt
    resta testa+coda + un puntatore esplicito a `nexus_search_semantic`. Cosi'
    il dato non viene MAI perso a livello di sistema (regola H: niente
    troncamento distruttivo).

    Best-effort: se l'offload non e' disponibile (Qdrant down), degrada al
    troncamento classico ma con puntatore che lo segnala (build_pointer).
    """
    if len(text) <= max_chars:
        return text
    from . import context_offload

    offload = context_offload.offload_to_rag(
        _embeddings, text, source_kind=source_kind, metadata=metadata,
    )
    head_size = max_chars // 5
    tail_size = max(200, max_chars - head_size - 200)
    pointer = context_offload.build_pointer(len(text), offload)
    return text[:head_size] + pointer + text[-tail_size:]


def _estimate_context_chars(messages: list) -> int:
    """Stima il numero totale di caratteri nel contesto messaggi."""
    total = 0
    for m in messages:
        if hasattr(m, "content") and isinstance(m.content, str):
            total += len(m.content)
        kwargs = getattr(m, "additional_kwargs", {}) or {}
        for block in kwargs.get("anthropic_content", []):
            if isinstance(block, dict):
                c = block.get("content", "")
                total += len(c) if isinstance(c, str) else 0
    return total

# ── Configurazione self-reflection (Fase 2) ──────────────────────────────────
# Tutti i parametri sono letti ESCLUSIVAMENTE dal DB tramite reflection_config.
# Nessuna variabile d'ambiente: le modifiche admin sono attive entro 60 secondi
# senza rideploy del servizio.

# Riferimenti ai servizi globali — iniettati da graph.py dopo l'inizializzazione
_providers: ProviderRegistry | None = None
_router: SemanticRouter | None = None
_embeddings: EmbeddingService | None = None
_storage: LocalLearningStorage | None = None
_retriever: InteractionRetriever | None = None
_tool_runner: ToolRunnerClient | None = None
_agent_router: AgentRouterClient | None = None
# PR-D: classifier agentico LLM per il gating adattivo del planner forte.
_agentic_classifier: Any = None

# ── Learning config (DB-backed, cache 60s) ───────────────────────────────────
# Letti dalla tabella `settings`: learning_auto_extract e learning_min_confidence.
# Stessa convenzione di reflection_config: nessuna env var, nessun hardcode,
# fallback conservativo se DB non raggiungibile.
import threading as _threading

_learning_cfg_lock = _threading.Lock()
_learning_cfg_cache: dict[str, Any] | None = None
_learning_cfg_ts: float = 0.0
_LEARNING_CFG_TTL = 60.0
_LEARNING_CFG_DEFAULTS: dict[str, Any] = {
    "auto_extract": True,
    "min_confidence": 0.6,
}


def _get_learning_config() -> dict[str, Any]:
    """Restituisce {auto_extract: bool, min_confidence: float} dal DB con cache 60s.

    Se DB non raggiungibile o psycopg2 mancante, usa i safe_defaults conservativi
    (auto_extract=True, min_confidence=0.6) e mantiene l'ultima cache valida.
    """
    global _learning_cfg_cache, _learning_cfg_ts
    import os

    now = time.monotonic()
    with _learning_cfg_lock:
        if _learning_cfg_cache is not None and now - _learning_cfg_ts < _LEARNING_CFG_TTL:
            return _learning_cfg_cache

    database_url = os.environ.get("DATABASE_URL", "")
    if not database_url:
        logger.warning("learning_config: DATABASE_URL non impostato, uso safe_defaults")
        with _learning_cfg_lock:
            _learning_cfg_cache = dict(_LEARNING_CFG_DEFAULTS)
            _learning_cfg_ts = now
        return dict(_LEARNING_CFG_DEFAULTS)

    try:
        import psycopg2  # type: ignore[import-untyped]
        conn = psycopg2.connect(database_url)
        try:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT key, value FROM settings "
                    "WHERE key IN ('learning_auto_extract','learning_min_confidence')"
                )
                rows = dict(cur.fetchall())
        finally:
            conn.close()
        cfg: dict[str, Any] = {
            "auto_extract": rows.get("learning_auto_extract", "true").strip().lower() != "false",
            "min_confidence": float(rows.get("learning_min_confidence", "0.6") or "0.6"),
        }
    except Exception as exc:
        logger.error("learning_config: errore lettura DB: %s — uso cache o defaults", exc)
        with _learning_cfg_lock:
            return dict(_learning_cfg_cache) if _learning_cfg_cache is not None else dict(_LEARNING_CFG_DEFAULTS)

    with _learning_cfg_lock:
        _learning_cfg_cache = cfg
        _learning_cfg_ts = now
    return cfg


def configure_services(
    providers: Any,
    router: Any,
    embeddings: Any,
    storage: Any,
    retriever: Any,
    tool_runner: Any = None,
    agent_router: Any = None,
    agentic_classifier: Any = None,
) -> None:
    """Inietta i servizi globali nei nodi. Chiamato da create_agent_graph()."""
    global _providers, _router, _embeddings, _storage, _retriever
    global _tool_runner, _agent_router, _agentic_classifier
    _providers = providers
    _router = router
    _embeddings = embeddings
    _storage = storage
    _retriever = retriever
    _tool_runner = tool_runner
    _agent_router = agent_router
    _agentic_classifier = agentic_classifier


# ─── RAG helper (BP7) ───────────────────────────────────────────────────────

# Intent per i quali il RAG inline e' utile: task che operano su codice/repo
# e tipicamente beneficiano del ricordo di task simili passati.
_RAG_INTENTS = {"code", "code_edit", "code_read", "refactor", "analyze",
                "fix", "implement", "debug", "review"}

# Soglia minima di similarita' per includere un'interazione nel contesto.
# Sotto questa soglia il match non e' significativo e introdurrebbe rumore.
_RAG_MIN_SCORE = 0.5


def _rag_top_k() -> int:
    """Numero massimo di interazioni recuperate per turno (DB-driven, mig 0217).

    Default alzato a 12 (era hardcoded 5): con l'offload lossless il RAG e' la
    fonte di verita' del contenuto troncato, quindi il recupero non deve essere
    artificialmente stretto. Fallback safe se DB down.
    """
    from . import context_offload

    return int(context_offload._load_offload_config()["rag_top_k"])


def _rag_snippet_max_chars() -> int:
    """Limite caratteri per snippet RAG incluso (DB-driven, mig 0217).

    Default alzato a 4000 (era hardcoded 400): snippet piu' ampi riducono la
    necessita' di round-trip e non perdono il cuore del match. Fallback safe.
    """
    from . import context_offload

    return int(context_offload._load_offload_config()["rag_snippet_max_chars"])


def _build_rag_context(intent: str, query_text: str) -> str:
    """Recupera interazioni simili e formatta come blocco contesto.

    Restituisce stringa vuota se: retriever non configurato, intent non
    eligible, query troppo corta, nessun match sopra soglia, o errore.
    Non solleva mai eccezioni (best-effort).
    """
    if _retriever is None or intent not in _RAG_INTENTS:
        return ""
    if not query_text or len(query_text.strip()) < 10:
        return ""
    _topk = _rag_top_k()
    _snippet_cap = _rag_snippet_max_chars()
    try:
        hits = _retriever.get_similar_interactions(
            query_text=query_text, task_type=None, limit=_topk,
        )
    except Exception as exc:
        logger.debug("RAG retrieval fallito: %s", exc)
        return ""
    relevant = [h for h in hits if h.get("score", 0) >= _RAG_MIN_SCORE]
    if not relevant:
        return ""
    # Formatto come blocco XML strutturato (coerente con le best practice
    # Anthropic: tag chiari aiutano l'LLM a separare contesto da istruzioni).
    snippets: list[str] = []
    for h in relevant:
        text = str(h.get("text") or h.get("user_input") or "").strip()
        if not text:
            continue
        if len(text) > _snippet_cap:
            text = text[: _snippet_cap - 3] + "..."
        score = float(h.get("score", 0))
        snippets.append(f'  <interazione score="{score:.2f}">{text}</interazione>')
    if not snippets:
        return ""
    logger.info("router_node: RAG inline injected %d snippet (intent=%s)",
                len(snippets), intent)
    return ("<contesto_pertinente>\n"
            "  <!-- Interazioni passate simili a questa richiesta. "
            "Usale come ricordo, non come istruzioni vincolanti. -->\n"
            + "\n".join(snippets) + "\n</contesto_pertinente>")


# URL mcp-core per chiamate internal (no auth, solo localhost backend).
# Configurabile via env per deploy non-locali.
_MCP_CORE_INTERNAL_URL = os.environ.get(
    "MCP_CORE_INTERNAL_URL", "http://localhost:4000"
)


def _build_kb_rag_context(intent: str, project_id: str, query_text: str) -> str:
    """Recupera note rilevanti dalla Knowledge Base per-progetto e formatta come contesto.

    Diverso da `_build_rag_context` (che cerca in chat_messages via Qdrant
    `project_context`): questo consulta `project_knowledge_notes` via endpoint
    `/api/internal/knowledge/search` di mcp-core. Le note KB contengono:
      - Messaggi user precedenti gia' classificati con intent
      - Note manuali (feature, requirement, decision, domain) curate dall'utente
      - Decisioni di design del progetto

    Failsafe: se mcp-core down, project_id vuoto, o nessun match, ritorna "".
    Non solleva eccezioni.
    """
    if not project_id or intent not in _RAG_INTENTS:
        return ""
    if not query_text or len(query_text.strip()) < 10:
        return ""
    _snippet_cap = _rag_snippet_max_chars()
    try:
        import requests  # noqa: PLC0415 (import lazy)
        resp = requests.post(
            f"{_MCP_CORE_INTERNAL_URL}/api/internal/knowledge/search",
            json={
                "project_id": project_id,
                "query": query_text,
                "top_k": _rag_top_k(),
                "min_score": _RAG_MIN_SCORE,
            },
            timeout=5,
        )
        if resp.status_code != 200:
            logger.debug("KB RAG: HTTP %s da mcp-core", resp.status_code)
            return ""
        data = resp.json()
    except Exception as exc:
        logger.debug("KB RAG retrieval fallito: %s", exc)
        return ""

    results = data.get("results", [])
    if not results:
        return ""

    snippets: list[str] = []
    for r in results:
        title = str(r.get("title") or "").strip()
        snippet_text = str(r.get("snippet") or "").strip()
        if not title and not snippet_text:
            continue
        intent_attr = r.get("intent") or "chat"
        score = float(r.get("score") or 0)
        if len(snippet_text) > _snippet_cap:
            snippet_text = snippet_text[: _snippet_cap - 3] + "..."
        snippets.append(
            f'  <nota intent="{intent_attr}" score="{score:.2f}">\n'
            f'    <titolo>{title}</titolo>\n'
            f'    <contenuto>{snippet_text}</contenuto>\n'
            f'  </nota>'
        )
    if not snippets:
        return ""

    logger.info("router_node: KB-RAG injected %d note (intent=%s, project=%s)",
                len(snippets), intent, project_id[:8])
    return ("<knowledge_base_progetto>\n"
            "  <!-- Note dal Knowledge Base del progetto: contesto, decisioni,\n"
            "       requirement, e messaggi simili gia' affrontati. Usa per\n"
            "       evitare duplicazioni e mantenere coerenza. -->\n"
            + "\n".join(snippets) + "\n</knowledge_base_progetto>")


# ─── Nodo: router ────────────────────────────────────────────────────────────

async def router_node(state: AgentState) -> dict[str, Any]:
    """Classifica l'intent utente e prepara il routing del modello.

    Passi:
    1. Classifica l'intent (semantico).
    2. Se `profile_name` e' esplicito, lo usa senza consultare Q-learning.
    3. Altrimenti: se e' configurato `AgentRouterClient`, invoca
       `select_agent(task_type=intent, instructions=testo)` e usa il
       profilo suggerito. Fallback al mapping statico intent→profilo.
    4. Inietta `system_text` dal `prompt_registry` e filtra `tools_json`.
    """
    messages = state.get("messages", [])
    if not messages:
        return {
            "user_intent": "chat",
            "task_type": "chat",
            "behavior_mode": state.get("behavior_mode", "bilanciata"),
            "token_budget": 400,
            "iterations": state.get("iterations", 0) + 1,
        }

    last_message = messages[-1]
    text = last_message.content if hasattr(last_message, "content") else str(last_message)

    # Stima token budget (approssimazione: 1 token ~ 4 caratteri)
    token_budget = max(400, len(text) // 4)

    if _router is not None:
        classification = _router.classify_intent(str(text))
        intent = classification.get("intent", "chat")
        # Confidence ritornata come stringa "0.xx" dal semantic router.
        # Defaults a 1.0 quando il classifier non popola il campo (es. fallback
        # keyword puro) cosi' clarify_or_expand_node non si attiva spuriamente.
        try:
            intent_confidence = float(classification.get("confidence", "1.0"))
        except (TypeError, ValueError):
            intent_confidence = 1.0
    else:
        intent = "chat"
        intent_confidence = 1.0

    # ── Fast-path conversazionale ────────────────────────────────────────
    # Euristica: messaggi corti puramente conversazionali (saluti, ringraziamenti,
    # affermazioni brevi) vengono spesso classificati erroneamente come "file_ops"
    # o "implement" se la conversazione precedente conteneva quel contesto. Il
    # classifier vede l'intera storia e si lascia influenzare. Forziamo "chat"
    # quando il messaggio matcha pattern conversazionali puri: cosi' il flow
    # bypassa tool/RAG/planner e l'executor risponde direttamente.
    _text_stripped = str(text).strip().lower().rstrip("!?.,;:")
    _CHAT_TOKENS = {
        "ciao", "salve", "buongiorno", "buonasera", "buonanotte", "hey",
        "hi", "hello", "ehi", "ehila", "yo",
        "grazie", "grazie mille", "thanks", "thank you", "ok", "okay",
        "perfetto", "fantastico", "ottimo", "bene", "capito",
        "addio", "arrivederci", "a presto", "ci sentiamo", "bye",
    }
    is_conversational = (
        len(_text_stripped) < 60
        and (
            _text_stripped in _CHAT_TOKENS
            or any(_text_stripped.startswith(tok + " ") for tok in _CHAT_TOKENS)
            or _text_stripped in {
                "come stai", "come va", "tutto bene", "tutto ok",
                "come ti chiami", "chi sei",
            }
        )
    )
    if is_conversational and intent not in ("chat", "general_chat"):
        logger.info(
            "router_node: fast-path conversazionale (msg=%r len=%d): override intent %s -> chat",
            _text_stripped[:40], len(_text_stripped), intent,
        )
        intent = "chat"
        intent_confidence = 1.0

    # ── Override deterministico: scaffolding applicativo ─────────────────────
    # Se il messaggio chiede di CREARE un'applicazione/progetto (famiglia
    # verbo+oggetto), forziamo intent="architecture" anche quando l'embedding
    # classifier l'ha mandato su code_read/docs. Questo evita che il profilo di
    # sola lettura blocchi lo scaffolding (il modello esplora invece di
    # scrivere). Non sovrascriviamo gli intent gia' "attivi" (architecture,
    # system_admin, file_ops): se siamo gia' su uno di quelli il routing e'
    # corretto. Speculare alla regola dell'agentic classifier, ma deterministico.
    if intent not in ("architecture", "system_admin", "file_ops") and _detect_scaffolding_intent(str(text)):
        logger.info(
            "router_node: override scaffolding deterministico, intent %s -> architecture (msg=%r)",
            intent, str(text)[:60],
        )
        intent = "architecture"
        intent_confidence = 0.9

    # ── PR-D: classifier agentico per il gating adattivo del planner forte ────
    # Solo se adaptive_classifier_enabled. Produce complexity/agentic_score/
    # is_ambiguous che route_after_router usa per decidere se attivare il
    # planner forte. Su fallback/timeout/non-conversazionale resta il path
    # keyword (zero regressioni). Cache TTL 24h + timeout interni al classifier.
    task_complexity: str | None = None
    agentic_score_val: float | None = None
    is_ambiguous_val: bool | None = None
    if not is_conversational and _agentic_classifier is not None:
        try:
            from . import orchestrator_config
            if orchestrator_config.adaptive_classifier_enabled():
                ag = await _agentic_classifier.classify(str(text))
                if ag is not None and not getattr(ag, "fallback_used", False):
                    task_complexity = getattr(ag, "complexity", None)
                    agentic_score_val = getattr(ag, "agentic_score", None)
                    is_ambiguous_val = getattr(ag, "is_ambiguous", None)
                    # L'intent LLM rimpiazza il keyword solo se valido.
                    _ag_intent = getattr(ag, "intent", None)
                    if _ag_intent:
                        intent = _ag_intent
                    _ag_conf = getattr(ag, "confidence", None)
                    if _ag_conf is not None:
                        intent_confidence = float(_ag_conf)
                    logger.info(
                        "router_node: adaptive classifier -> intent=%s complexity=%s "
                        "agentic=%.2f ambiguous=%s",
                        intent, task_complexity, agentic_score_val or 0.0, is_ambiguous_val,
                    )
        except Exception as _ag_exc:
            logger.debug("router_node: adaptive classifier skip (%s)", _ag_exc)

    behavior_mode = state.get("behavior_mode", "bilanciata")

    # Boost token_budget per intent complessi che richiedono piu' output
    # (generazione documenti, analisi codice, fix estesi).
    COMPLEX_INTENTS = {"doc_generate", "analyze", "fix", "refactor", "implement"}
    text_lower = str(text).lower()
    is_complex = intent in COMPLEX_INTENTS or any(
        kw in text_lower for kw in ("genera", "document", "analisi tecnica", "implementa")
    )
    if is_complex:
        token_budget = max(token_budget, 4096)

    # ── Profile selection ────────────────────────────────────────────────
    # Se il chiamante non ha forzato un profile_name, lo deriviamo dall'intent.
    # Il profile decide: system_text (da prompt_registry), allowed_tools.
    explicit_profile = state.get("profile_name")
    profile = None
    if explicit_profile:
        profile = profile_loader.get_profile(explicit_profile)
        if profile is None:
            logger.warning("router_node: profile_name='%s' sconosciuto, ignoro",
                           explicit_profile)
    if profile is None and _agent_router is not None:
        # Consultiamo il Q-Learning router (sub-nodo): best-effort, se il
        # canale gRPC non risponde ignoriamo silenziosamente.
        try:
            last_text = str(text)[:4096]
            sel = await _agent_router.select_agent(
                task_type=intent, instructions=last_text,
                task_id=state.get("thread_id"),
            )
            if not sel.is_empty:
                suggested = profile_loader.get_profile(sel.agent_type)
                if suggested is not None:
                    logger.info(
                        "router_node: agent_router suggerisce profile=%s "
                        "strategy=%s q_value=%.3f conf=%.3f",
                        suggested.name, sel.strategy, sel.q_value, sel.confidence,
                    )
                    profile = suggested
                else:
                    logger.debug(
                        "router_node: agent_router propone profile '%s' "
                        "non presente nel catalog",
                        sel.agent_type,
                    )
        except Exception as exc:
            logger.debug("router_node: agent_router select fallito: %s", exc)

    if profile is None:
        profile = profile_loader.route_profile_for_intent(intent)

    # Mig 0181 — Calcola budget iter adattivo se non già impostato (primo turno).
    # Prompt complesso (fullstack scaffolding, refactor end-to-end) -> budget alto.
    # Prompt semplice (chat, fix mirato) -> budget basso. Sostituisce il
    # MAX_AGENT_ITERATIONS costante in route_after_executor.
    iter_budget_existing = int(state.get("iteration_budget") or 0)
    if iter_budget_existing <= 0:
        # Primo messaggio del turno: prende il content dell'ULTIMO HumanMessage
        # (quello che ha appena innescato il run, il piu' rappresentativo del task).
        last_human_text = ""
        for m in reversed(messages):
            if hasattr(m, "type") and m.type == "human":
                content = getattr(m, "content", "")
                if isinstance(content, list):
                    content = " ".join(
                        b.get("text", "") for b in content if isinstance(b, dict)
                    )
                last_human_text = str(content)
                break
        initial_model = state.get("model_override") or state.get("sticky_model")
        iter_budget, complexity_score = compute_iteration_budget(last_human_text, initial_model)
        logger.info(
            "router_node: adaptive budget iter=%d complexity=%d model=%s prompt_len=%d",
            iter_budget, complexity_score, initial_model or "?", len(last_human_text)
        )
    else:
        # Iterazioni successive: mantiene il budget calcolato al primo turno.
        iter_budget = iter_budget_existing
        complexity_score = int(state.get("complexity_score") or 0)

    updates: dict[str, Any] = {
        "user_intent": intent,
        "task_type": intent,
        "behavior_mode": behavior_mode,
        "token_budget": token_budget,
        "iterations": state.get("iterations", 0) + 1,
        "intent_confidence": intent_confidence,
        "iteration_budget": iter_budget,
        "complexity_score": complexity_score,
    }
    # PR-D: segnali del classifier agentico per il gating adattivo (se prodotti).
    if task_complexity is not None:
        updates["task_complexity"] = task_complexity
    if agentic_score_val is not None:
        updates["agentic_score"] = agentic_score_val
    if is_ambiguous_val is not None:
        updates["is_ambiguous"] = is_ambiguous_val

    if profile is not None:
        updates["profile_name"] = profile.name
        # Se il chiamante non ha gia' passato un system_text, lo prendiamo
        # dal registry tramite la prompt_key del profilo. Non sovrascriviamo
        # un system_text esplicito per preservare override utente.
        if not state.get("system_text"):
            resolved = prompt_registry.get_prompt(profile.prompt_key)
            if resolved:
                # Sostituisce placeholder runtime ({{lang_hint}}, {{type_hint}},
                # {{repo_summary}}). Mai lasciare {{...}} letterale al provider.
                rendered = prompt_renderer.render(resolved, dict(state), intent)
                # ── RAG inline (BP7 piano riduzione token) ─────────────────
                # Per intent code-related, recupera interazioni simili da Qdrant
                # e prependi come contesto pertinente. Riduce le tool call
                # ridondanti (read_file dello stesso file) su task ricorrenti.
                rag_block = _build_rag_context(intent, str(text))
                if rag_block:
                    rendered = rag_block + "\n\n" + rendered
                # KB inline: cerca note rilevanti nella Knowledge Base del progetto.
                # Diverso da _build_rag_context (interazioni chat passate):
                # qui prendiamo note auto+manuali con context specifico del progetto.
                kb_block = _build_kb_rag_context(intent, str(state.get("project_id") or ""), str(text))
                if kb_block:
                    rendered = kb_block + "\n\n" + rendered
                # PR-3 Codex pattern: project_instructions.md injection nel system_text.
                # PR-3 Cursor pattern: <available_subagents> block per auto-delegation.
                try:
                    extra_blocks = _build_pr3_system_blocks(state)
                    if extra_blocks:
                        rendered = rendered + "\n\n" + extra_blocks
                except Exception as _exc:
                    logger.debug("pr3 injection skip: %s", _exc)
                updates["system_text"] = rendered
        # Filtriamo tools_json combinando whitelist profilo + intent (BP5).
        # Il doppio filtraggio: per chat/analyze/review viene rimosso anche
        # cio' che il profilo permetterebbe ma che non serve all'intent.
        tools = state.get("tools_json") or []
        if tools:
            filtered = profile.filter_tools_for_intent(tools, intent)
            if len(filtered) != len(tools):
                logger.info(
                    "router_node: profile=%s intent=%s filtra tools %d -> %d",
                    profile.name, intent, len(tools), len(filtered),
                )
                updates["tools_json"] = filtered
        logger.info(
            "router_node: intent=%s token_budget=%d mode=%s profile=%s",
            intent, token_budget, behavior_mode, profile.name,
        )
    else:
        # Nessun profilo trovato: applica comunque _INTENT_TOOL_SUBSET (BP5 fallback).
        # Senza profilo il filtro combinato profilo×intent non scatta, ma possiamo
        # applicare il subset intent-only direttamente: code_read riceve 6 tool
        # di sola lettura invece di tutti i 31 tool builtin. Riduce il payload
        # di ogni turno del loop del 70-80%.
        tools = state.get("tools_json") or []
        if tools and intent:
            intent_subset = profile_loader._INTENT_TOOL_SUBSET.get(intent)
            if intent_subset is not None and intent_subset != ["*"]:
                _always_on = profile_loader.AgentProfile._ALWAYS_ON_TOOLS
                allow = set(intent_subset) | _always_on
                filtered_np = [t for t in tools if t.get("name") in allow]
                if len(filtered_np) != len(tools):
                    logger.info(
                        "router_node: intent=%s filtra tools %d -> %d (no-profile fallback)",
                        intent, len(tools), len(filtered_np),
                    )
                    updates["tools_json"] = filtered_np
        logger.info(
            "router_node: intent=%s token_budget=%d mode=%s profile=<none>",
            intent, token_budget, behavior_mode,
        )

    # ── Meta-step `routing` per pubblicazione in chat ───────────────────────
    # Emesso solo se la classificazione e' significativa (no chat banale a 0
    # confidence senza profilo) e se il flag `meta_steps.routing_enabled`
    # e' attivo. Riduce rumore nelle conversazioni di small-talk.
    if profile is not None or intent != "chat":
        profile_name = profile.name if profile is not None else None
        title = f"Intent: {intent}"
        if profile_name:
            title += f" — profilo {profile_name}"
        routing_meta = meta_steps.make(
            kind="routing",
            title=title,
            payload={
                "intent": intent,
                "task_type": intent,
                "profile_name": profile_name,
                "behavior_mode": behavior_mode,
                "token_budget": token_budget,
            },
        )
        if routing_meta:
            updates["meta_steps"] = [routing_meta]
            meta_steps.persist_async(state.get("thread_id"), routing_meta)

    # ── Nexus thinking (visibilita' processo decisionale) ──────────────────
    try:
        _profile_name = profile.name if profile is not None else None
        _thinking_lines: list[str] = []
        _thinking_lines.append(
            f"Classificato intent '{intent}' (confidence {intent_confidence:.2f})."
        )
        _thinking_lines.append(
            f"Modalita' comportamento: {behavior_mode}, token budget stimato {token_budget}."
        )
        if complexity_score:
            _thinking_lines.append(
                f"Complessita' stimata del task: {complexity_score} punti, budget iterazioni {iter_budget}."
            )
        if _profile_name:
            _thinking_lines.append(f"Profilo agente selezionato: {_profile_name}.")
        _emit_thinking(updates, *_thinking_lines)
    except Exception as _thinking_exc:
        logger.debug("router_node: thinking emit fallito: %s", _thinking_exc)

    return updates


# ─── Nodo: executor ──────────────────────────────────────────────────────────

def _dedup_tool_results(messages: list[Any]) -> list[Any]:
    """Dedup semantico dei tool_result duplicati (BP11 piano riduzione token).

    Quando lo stesso file viene letto piu' volte (frequente in sessioni
    edit-heavy: read_file + edit + read_file di nuovo per verifica), la
    history accumula copie del contenuto. Manteniamo solo l'ULTIMA copia
    e sostituiamo le precedenti con un marker compatto che cita il msg
    successivo.

    Hash key: SHA-1 di (content_normalizzato[:256]). Non includiamo il
    tool_use_id (cambia ad ogni invocazione) perche' vogliamo proprio
    intercettare il caso in cui invocazioni diverse producono lo stesso
    output.

    NB: il tool_use_id del blocco tool_result resta invariato -- Anthropic
    valida l'accoppiamento tool_use<->tool_result solo sull'id, non sul
    content. Quindi modificare il content e' sicuro.
    """
    import hashlib

    # Prima passata: mappa hash -> indice messaggio dell'ultima occorrenza.
    # last_indices[h] = (msg_idx, block_idx)
    last_indices: dict[str, tuple[int, int]] = {}
    for mi, m in enumerate(messages):
        extra = getattr(m, "additional_kwargs", {}) or {}
        blocks = extra.get("anthropic_content")
        if not isinstance(blocks, list):
            continue
        for bi, block in enumerate(blocks):
            if not isinstance(block, dict) or block.get("type") != "tool_result":
                continue
            content = block.get("content", "")
            if isinstance(content, list):
                content = " ".join(
                    str(b.get("text", "")) for b in content
                    if isinstance(b, dict) and b.get("type") == "text"
                )
            if not isinstance(content, str) or len(content) < 200:
                continue  # skip content piccoli: dedup non porta beneficio
            normalized = content.strip()[:256]
            h = hashlib.sha1(normalized.encode("utf-8", errors="ignore")).hexdigest()[:16]
            last_indices[h] = (mi, bi)

    # Seconda passata: sostituisci le occorrenze NON ultime con marker.
    deduped_count = 0
    new_messages = []
    for mi, m in enumerate(messages):
        extra = getattr(m, "additional_kwargs", {}) or {}
        blocks = extra.get("anthropic_content")
        if not isinstance(blocks, list):
            new_messages.append(m)
            continue
        changed = False
        new_blocks = []
        for bi, block in enumerate(blocks):
            if not isinstance(block, dict) or block.get("type") != "tool_result":
                new_blocks.append(block)
                continue
            content = block.get("content", "")
            if isinstance(content, list):
                serialized = " ".join(
                    str(b.get("text", "")) for b in content
                    if isinstance(b, dict) and b.get("type") == "text"
                )
            elif isinstance(content, str):
                serialized = content
            else:
                new_blocks.append(block)
                continue
            if not serialized or len(serialized) < 200:
                new_blocks.append(block)
                continue
            normalized = serialized.strip()[:256]
            h = hashlib.sha1(normalized.encode("utf-8", errors="ignore")).hexdigest()[:16]
            last_mi, last_bi = last_indices.get(h, (mi, bi))
            if (mi, bi) != (last_mi, last_bi):
                # Non e' l'ultima occorrenza: sostituisci con marker.
                new_blocks.append({
                    **block,
                    "content": f"[deduped: contenuto identico al tool_result piu' recente in msg #{last_mi}]",
                })
                changed = True
                deduped_count += 1
            else:
                new_blocks.append(block)
        if changed:
            new_msg = HumanMessage(
                content=getattr(m, "content", ""),
                additional_kwargs={"anthropic_content": new_blocks},
            )
            new_messages.append(new_msg)
        else:
            new_messages.append(m)

    if deduped_count > 0:
        logger.info("dedup_tool_results: rimosse %d copie duplicate", deduped_count)
    return new_messages


def _compress_marker(content: str) -> str:
    """Offload LOSSLESS del contenuto completo + marker da appendere al placeholder.

    Chiamato da _compress_old_tool_results PRIMA di sostituire un tool_result
    vecchio con la sua versione compressa. Indicizza il contenuto intero in RAG
    (idempotente per hash) cosi' il dato resta recuperabile via
    `nexus_search_semantic` anche dopo la compressione nel prompt.

    Best-effort: se l'offload non e' disponibile, il marker segnala comunque la
    compressione (degraded). Non solleva mai.
    """
    from . import context_offload

    offload = context_offload.offload_to_rag(
        _embeddings, content, source_kind="tool_result_compressed",
    )
    if offload is not None:
        return (
            f"\n[... compresso: {len(content)} char originali INDICIZZATI in RAG "
            f"(ref={offload['ref'][:12]}). Recupera con nexus_search_semantic ...]"
        )
    return f"\n[... compresso: {len(content)} char originali ...]"


def _compress_old_tool_results(
    messages: list[Any],
    keep_recent: int = 6,
    max_content_chars: int = 500,
) -> list[Any]:
    """Comprime i tool_result dei messaggi piu' vecchi per ridurre il contesto.

    Mantiene intatti gli ultimi `keep_recent` messaggi. Per i precedenti,
    i blocchi tool_result con contenuto > `max_content_chars` char vengono
    sostituiti con un riassunto troncato.

    `max_content_chars` e' configurabile: nelle fasi di loop avanzato
    (iterazioni alte) l'executor passa un valore piu' basso (es. 150)
    per una compressione piu' aggressiva.

    Prima della compressione applica _dedup_tool_results: spesso la stessa
    risorsa viene letta piu' volte e il dedup risparmia tokens senza
    perdere informazione (l'ultima copia e' preservata).
    """
    # Step 0: dedup duplicati su tutta la history (anche keep_recent benefici).
    messages = _dedup_tool_results(messages)
    if len(messages) <= keep_recent:
        return messages

    compressed = []
    boundary = len(messages) - keep_recent
    # La finestra "recente" usa una soglia piu' permissiva: 2× max_content_chars.
    recent_threshold = max_content_chars * 2

    for i, m in enumerate(messages):
        if i >= boundary:
            # Anche i messaggi recenti vengono compressi, ma con soglia piu' alta.
            extra = getattr(m, "additional_kwargs", {}) or {}
            blocks = extra.get("anthropic_content")
            if blocks is None or not isinstance(blocks, list):
                compressed.append(m)
                continue
            changed = False
            new_blocks = []
            for block in blocks:
                if not isinstance(block, dict) or block.get("type") != "tool_result":
                    new_blocks.append(block)
                    continue
                content = block.get("content", "")
                if isinstance(content, str) and len(content) > recent_threshold:
                    kept = max(recent_threshold // 2, 200)
                    new_blocks.append({
                        **block,
                        "content": content[:kept] + _compress_marker(content),
                    })
                    changed = True
                else:
                    new_blocks.append(block)
            if changed:
                new_msg = HumanMessage(
                    content=getattr(m, "content", ""),
                    additional_kwargs={"anthropic_content": new_blocks},
                )
                compressed.append(new_msg)
            else:
                compressed.append(m)
            continue

        extra = getattr(m, "additional_kwargs", {}) or {}
        blocks = extra.get("anthropic_content")
        if blocks is None or not isinstance(blocks, list):
            compressed.append(m)
            continue

        changed = False
        new_blocks = []
        for block in blocks:
            if not isinstance(block, dict) or block.get("type") != "tool_result":
                new_blocks.append(block)
                continue
            content = block.get("content", "")
            if isinstance(content, str) and len(content) > max_content_chars:
                kept = max(max_content_chars // 2, 100)
                new_blocks.append({
                    **block,
                    "content": content[:kept] + _compress_marker(content),
                })
                changed = True
            else:
                new_blocks.append(block)

        if changed:
            new_msg = HumanMessage(
                content=getattr(m, "content", ""),
                additional_kwargs={"anthropic_content": new_blocks},
            )
            compressed.append(new_msg)
        else:
            compressed.append(m)

    return compressed


# ────────────────────────────────────────────────────────────────────────────
# FIX A-D (ADR 0014) — Context size management
# ────────────────────────────────────────────────────────────────────────────
# Settings DB (mig 0199), cache 60s. Fallback safe se DB down.

_CTX_MGMT_CACHE: dict[str, Any] = {"loaded_at": 0.0, "config": None}
_CTX_MGMT_TTL_SEC = 60.0
_CTX_MGMT_DEFAULTS: dict[str, Any] = {
    "compress_start_iter": 5,
    "compress_phase_boundaries": [5, 10, 20, 50],
    "compress_phase_keep_recent": [8, 5, 3, 2],
    "compress_phase_max_chars": [2000, 1000, 500, 150],
    "dedup_tool_results_enabled": True,
    "drop_unused_base64_age": 3,
    "predictive_cap_ratio": 0.5,
}


def _load_ctx_mgmt_config() -> dict[str, Any]:
    """Carica i settings agent.context.* dal DB con cache 60s.

    Fallback ai defaults safe se il DB e' down (mai solleva).
    """
    now = time.time()
    cached = _CTX_MGMT_CACHE["config"]
    if cached is not None and (now - _CTX_MGMT_CACHE["loaded_at"]) < _CTX_MGMT_TTL_SEC:
        return cached  # type: ignore[return-value]

    config = {
        "compress_start_iter": int(_CTX_MGMT_DEFAULTS["compress_start_iter"]),
        "compress_phase_boundaries": list(_CTX_MGMT_DEFAULTS["compress_phase_boundaries"]),
        "compress_phase_keep_recent": list(_CTX_MGMT_DEFAULTS["compress_phase_keep_recent"]),
        "compress_phase_max_chars": list(_CTX_MGMT_DEFAULTS["compress_phase_max_chars"]),
        "dedup_tool_results_enabled": bool(_CTX_MGMT_DEFAULTS["dedup_tool_results_enabled"]),
        "drop_unused_base64_age": int(_CTX_MGMT_DEFAULTS["drop_unused_base64_age"]),
        "predictive_cap_ratio": float(_CTX_MGMT_DEFAULTS["predictive_cap_ratio"]),
    }
    try:
        import os as _os
        import psycopg2  # type: ignore[import-untyped]
        db_url = _os.environ.get("DATABASE_URL") or "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable"
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT key, value FROM settings WHERE key LIKE 'agent.context.%%'"
                )
                rows = cur.fetchall()
        for key, value in rows:
            sval = str(value).strip().strip('"')
            try:
                if key == "agent.context.compress_start_iter":
                    config["compress_start_iter"] = max(1, int(sval))
                elif key == "agent.context.compress_phase_boundaries":
                    config["compress_phase_boundaries"] = [int(x) for x in sval.split(",") if x.strip()]
                elif key == "agent.context.compress_phase_keep_recent":
                    config["compress_phase_keep_recent"] = [int(x) for x in sval.split(",") if x.strip()]
                elif key == "agent.context.compress_phase_max_chars":
                    config["compress_phase_max_chars"] = [int(x) for x in sval.split(",") if x.strip()]
                elif key == "agent.context.dedup_tool_results_enabled":
                    config["dedup_tool_results_enabled"] = sval.lower() not in ("false", "0", "off", "no")
                elif key == "agent.context.drop_unused_base64_age":
                    config["drop_unused_base64_age"] = max(0, int(sval))
                elif key == "agent.context.predictive_cap_ratio":
                    r = float(sval)
                    if 0.3 <= r <= 0.9:
                        config["predictive_cap_ratio"] = r
            except Exception as parse_exc:
                logger.warning("ctx_mgmt: parse setting %s fallito: %s", key, parse_exc)
    except Exception as exc:
        logger.warning("ctx_mgmt: load DB fallito, uso defaults (%s)", exc)

    # Sanity check coerenza fasi.
    lens = (
        len(config["compress_phase_boundaries"]),
        len(config["compress_phase_keep_recent"]),
        len(config["compress_phase_max_chars"]),
    )
    if len(set(lens)) != 1 or lens[0] == 0:
        logger.warning(
            "ctx_mgmt: fasi compressione incoerenti (%s), fallback ai defaults", lens
        )
        config["compress_phase_boundaries"] = list(_CTX_MGMT_DEFAULTS["compress_phase_boundaries"])
        config["compress_phase_keep_recent"] = list(_CTX_MGMT_DEFAULTS["compress_phase_keep_recent"])
        config["compress_phase_max_chars"] = list(_CTX_MGMT_DEFAULTS["compress_phase_max_chars"])

    _CTX_MGMT_CACHE["config"] = config
    _CTX_MGMT_CACHE["loaded_at"] = now
    return config


# ── FIX A: compressione anticipata escalante ────────────────────────────────

def _should_compress_now(iteration: int, settings: dict[str, Any] | None = None) -> tuple[bool, dict[str, int]]:
    """Decide se comprimere in base all'iterazione corrente e con quali parametri.

    Ritorna (compress, params) dove params = {"keep_recent": int, "max_content_chars": int}.

    Logica (default): iter < 5 -> no. 5-9 -> (8, 2000). 10-19 -> (5, 1000).
    20-49 -> (3, 500). >= 50 -> (2, 150). I boundary sono DB-driven (mig 0199).
    """
    cfg = settings or _load_ctx_mgmt_config()
    start = int(cfg["compress_start_iter"])
    if iteration < start:
        return False, {"keep_recent": 0, "max_content_chars": 0}
    boundaries = list(cfg["compress_phase_boundaries"])
    keeps = list(cfg["compress_phase_keep_recent"])
    chars = list(cfg["compress_phase_max_chars"])
    # Sceglie la fase la cui boundary e' la massima <= iteration.
    idx = 0
    for i, b in enumerate(boundaries):
        if iteration >= b:
            idx = i
        else:
            break
    return True, {
        "keep_recent": int(keeps[idx]),
        "max_content_chars": int(chars[idx]),
    }


# ── FIX B: deduplicazione tool_result identici per signature ────────────────

def _tool_use_signature(tool_name: str, args: Any) -> str:
    """Signature stabile: sha256(tool_name + json(args, sort_keys=True))[:16]."""
    try:
        args_json = json.dumps(args, sort_keys=True, default=str, ensure_ascii=False)
    except Exception:
        args_json = str(args)
    payload = f"{tool_name}|{args_json}".encode("utf-8", errors="ignore")
    return hashlib.sha256(payload).hexdigest()[:16]


def _dedup_tool_results_history(messages: list[Any]) -> list[Any]:
    """Per ogni signature (tool_name+args) tiene solo l'ULTIMO tool_result.

    Le occorrenze precedenti vengono sostituite con un placeholder che cita la
    presenza di una versione piu' recente piu' avanti nella history. Il
    tool_use_id viene preservato per non rompere l'accoppiamento Anthropic.

    Diverso da `_dedup_tool_results` (BP11) che hashea il CONTENT: qui hashiamo
    la CHIAMATA. Cosi' coglie il caso "stesso file letto 24 volte con stessi
    args" anche se il content differisce per timestamp/metadata.
    """
    # Step 1: indice signature -> (mi_tool_use, tool_use_id) -> ultima occorrenza tool_result.
    # Mappiamo tool_use_id -> signature (tool_use sta in messaggi AI precedenti).
    tool_use_id_to_sig: dict[str, str] = {}
    for m in messages:
        content = getattr(m, "content", None)
        if isinstance(content, list):
            for block in content:
                if isinstance(block, dict) and block.get("type") == "tool_use":
                    tid = str(block.get("id", "") or "")
                    if tid:
                        tool_use_id_to_sig[tid] = _tool_use_signature(
                            str(block.get("name", "") or ""),
                            block.get("input", {}) or {},
                        )
        extra = getattr(m, "additional_kwargs", {}) or {}
        anth = extra.get("anthropic_content")
        if isinstance(anth, list):
            for block in anth:
                if isinstance(block, dict) and block.get("type") == "tool_use":
                    tid = str(block.get("id", "") or "")
                    if tid:
                        tool_use_id_to_sig[tid] = _tool_use_signature(
                            str(block.get("name", "") or ""),
                            block.get("input", {}) or {},
                        )

    # Step 2: scorri history e individua ultimo tool_result per signature.
    last_pos_for_sig: dict[str, tuple[int, int]] = {}
    for mi, m in enumerate(messages):
        extra = getattr(m, "additional_kwargs", {}) or {}
        blocks = extra.get("anthropic_content")
        if not isinstance(blocks, list):
            continue
        for bi, block in enumerate(blocks):
            if not isinstance(block, dict) or block.get("type") != "tool_result":
                continue
            tid = str(block.get("tool_use_id", "") or "")
            sig = tool_use_id_to_sig.get(tid)
            if not sig:
                continue
            last_pos_for_sig[sig] = (mi, bi)

    # Step 3: sostituisci tool_result non-ultimi con placeholder.
    dedup_count = 0
    new_messages: list[Any] = []
    for mi, m in enumerate(messages):
        extra = getattr(m, "additional_kwargs", {}) or {}
        blocks = extra.get("anthropic_content")
        if not isinstance(blocks, list):
            new_messages.append(m)
            continue
        changed = False
        new_blocks = []
        for bi, block in enumerate(blocks):
            if not isinstance(block, dict) or block.get("type") != "tool_result":
                new_blocks.append(block)
                continue
            tid = str(block.get("tool_use_id", "") or "")
            sig = tool_use_id_to_sig.get(tid)
            if sig is None:
                new_blocks.append(block)
                continue
            last = last_pos_for_sig.get(sig)
            if last is None or last == (mi, bi):
                new_blocks.append(block)
                continue
            new_blocks.append({
                "type": "tool_result",
                "tool_use_id": tid,
                "content": (
                    "[dedup: stesso tool con stessi args, vedi risultato "
                    f"piu' recente in msg #{last[0]}]"
                ),
            })
            changed = True
            dedup_count += 1
        if changed:
            new_messages.append(HumanMessage(
                content=getattr(m, "content", ""),
                additional_kwargs={"anthropic_content": new_blocks},
            ))
        else:
            new_messages.append(m)

    if dedup_count > 0:
        logger.info(
            "dedup_tool_results_history: rimossi %d tool_result duplicati per signature",
            dedup_count,
        )
    return new_messages


# ── FIX C: drop body base64 da tool_result vecchi non citati ────────────────

_BASE64_RE = _re_budget.compile(r"[A-Za-z0-9+/=]")


def _looks_like_base64(s: str, min_len: int = 200) -> bool:
    """Heuristic: stringa lunga senza newline composta al 90%+ di char base64."""
    if not isinstance(s, str) or len(s) < min_len:
        return False
    if "\n" in s[:min_len]:
        return False
    # Conta su un campione per evitare scan completo su blob grandi.
    sample = s if len(s) <= 4096 else s[:4096]
    valid = sum(1 for c in sample if _BASE64_RE.match(c))
    return valid / max(len(sample), 1) >= 0.9


def _drop_unused_base64_payloads(messages: list[Any], max_age: int | None = None, keep_recent: int = 2) -> list[Any]:
    """Sostituisce body base64 di tool_result vecchi non citati con placeholder.

    Per ogni tool_result che ha content base64 (heuristic):
      - se nei `max_age` messaggi successivi i primi 16 char del base64
        NON appaiono testualmente, il body viene rimpiazzato con un placeholder.
      - gli ultimi `keep_recent` messaggi sono sempre preservati intatti.
    """
    cfg = _load_ctx_mgmt_config()
    if max_age is None:
        max_age = int(cfg["drop_unused_base64_age"])
    if max_age <= 0 or len(messages) <= keep_recent:
        return messages

    boundary = len(messages) - keep_recent
    dropped = 0
    new_messages: list[Any] = []

    # Pre-calcola testo cumulativo per ogni indice (only-text) per ricerca veloce.
    text_per_msg: list[str] = []
    for m in messages:
        parts: list[str] = []
        c = getattr(m, "content", "")
        if isinstance(c, str):
            parts.append(c)
        elif isinstance(c, list):
            for b in c:
                if isinstance(b, dict) and b.get("type") == "text":
                    parts.append(str(b.get("text", "")))
        extra = getattr(m, "additional_kwargs", {}) or {}
        anth = extra.get("anthropic_content")
        if isinstance(anth, list):
            for b in anth:
                if isinstance(b, dict):
                    bc = b.get("content")
                    if isinstance(bc, str):
                        parts.append(bc)
        text_per_msg.append(" ".join(parts))

    for mi, m in enumerate(messages):
        if mi >= boundary:
            new_messages.append(m)
            continue
        extra = getattr(m, "additional_kwargs", {}) or {}
        blocks = extra.get("anthropic_content")
        if not isinstance(blocks, list):
            new_messages.append(m)
            continue
        changed = False
        new_blocks = []
        for block in blocks:
            if not isinstance(block, dict) or block.get("type") != "tool_result":
                new_blocks.append(block)
                continue
            content = block.get("content", "")
            if not isinstance(content, str) or not _looks_like_base64(content):
                new_blocks.append(block)
                continue
            prefix = content[:16]
            window_hi = min(len(messages), mi + 1 + max_age)
            cited = False
            for j in range(mi + 1, window_hi):
                if prefix in text_per_msg[j]:
                    cited = True
                    break
            if cited:
                new_blocks.append(block)
                continue
            orig_len = len(content)
            new_blocks.append({
                **block,
                "content": (
                    f"[contenuto base64 originale di {orig_len} byte rimosso "
                    f"dalla history per ottimizzazione context. Se serve "
                    f"rileggilo con il tool originale.]"
                ),
            })
            changed = True
            dropped += 1
        if changed:
            new_messages.append(HumanMessage(
                content=getattr(m, "content", ""),
                additional_kwargs={"anthropic_content": new_blocks},
            ))
        else:
            new_messages.append(m)

    if dropped > 0:
        logger.info(
            "drop_unused_base64_payloads: rimossi %d body base64 inutilizzati",
            dropped,
        )
    return new_messages


# ── FIX D: predictive context cap pre-tool ──────────────────────────────────

_CONTEXT_WINDOW_CACHE: dict[str, tuple[float, int]] = {}
_CONTEXT_WINDOW_TTL_SEC = 120.0


def _model_context_window(model: str) -> int:
    """Legge ai_price_catalog.context_window per il modello. Cache 120s.

    Fallback safe 128_000 se DB down o modello non in catalogo.
    """
    if not model:
        return 128_000
    now = time.time()
    entry = _CONTEXT_WINDOW_CACHE.get(model)
    if entry and (now - entry[0]) < _CONTEXT_WINDOW_TTL_SEC:
        return entry[1]
    window = 128_000
    try:
        import os as _os
        import psycopg2  # type: ignore[import-untyped]
        db_url = _os.environ.get("DATABASE_URL") or "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable"
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT context_window FROM ai_price_catalog "
                    "WHERE model = %s AND is_enabled = true "
                    "ORDER BY effective_from DESC LIMIT 1",
                    (model,),
                )
                row = cur.fetchone()
                if row and row[0]:
                    window = int(row[0])
    except Exception as exc:
        logger.debug("_model_context_window: lookup DB fallito per %s: %s", model, exc)
    _CONTEXT_WINDOW_CACHE[model] = (now, window)
    return window


def _estimate_tool_result_size_bytes(tool_name: str, args: dict[str, Any]) -> int:
    """Stima upper-bound dei byte attesi nel tool_result.

    Heuristiche per i tool noti, con overhead 1.4× per espansione base64.
    """
    if not isinstance(args, dict):
        args = {}
    if tool_name in ("nexus_read_attachment", "nexus_read_archive_entry"):
        length = args.get("length")
        try:
            length_i = int(length) if length is not None else 102_400
        except Exception:
            length_i = 102_400
        encoding = str(args.get("encoding", "auto") or "auto").lower()
        overhead = 1.4 if encoding in ("auto", "base64") else 1.05
        return int(length_i * overhead)
    if tool_name == "nexus_extract_pdf_text":
        return 100_000
    if tool_name in ("nexus_extract_docx_text", "nexus_extract_xlsx_data", "nexus_extract_figma_structure"):
        return 80_000
    if tool_name in ("nexus_list_archive_entries", "nexus_list_attachments", "nexus_inspect_attachment"):
        return 4_000
    if tool_name == "nexus_describe_image_attachment":
        return 8_000
    return 5_000


def _current_context_token_estimate(messages: list[Any], system_text: str = "") -> int:
    """Stima token totali del context attuale (~ chars/3.5)."""
    total_chars = len(system_text or "")
    for m in messages:
        c = getattr(m, "content", "")
        if isinstance(c, str):
            total_chars += len(c)
        elif isinstance(c, list):
            for b in c:
                if isinstance(b, dict):
                    for v in b.values():
                        if isinstance(v, str):
                            total_chars += len(v)
        extra = getattr(m, "additional_kwargs", {}) or {}
        anth = extra.get("anthropic_content")
        if isinstance(anth, list):
            for b in anth:
                if isinstance(b, dict):
                    for v in b.values():
                        if isinstance(v, str):
                            total_chars += len(v)
        elif isinstance(anth, str):
            total_chars += len(anth)
    return int(total_chars / 3.5)


def _predictive_cap_check(
    tool_name: str,
    args: dict[str, Any],
    messages: list[Any],
    model: str,
    system_text: str = "",
) -> str | None:
    """Decide se la chiamata al tool farebbe superare il ratio*context_window.

    Ritorna None se OK, altrimenti messaggio user-facing da iniettare come
    tool_result error.
    """
    cfg = _load_ctx_mgmt_config()
    ratio = float(cfg["predictive_cap_ratio"])
    window = _model_context_window(model)
    cap_tokens = int(window * ratio)
    current = _current_context_token_estimate(messages, system_text)
    expected_bytes = _estimate_tool_result_size_bytes(tool_name, args)
    expected_tokens = int(expected_bytes / 3.5)
    projected = current + expected_tokens
    if projected <= cap_tokens:
        return None
    pct = int(current / max(window, 1) * 100)
    logger.warning(
        "predictive_cap: tool=%s bloccato (current=%d tok, expected=+%d tok, "
        "projected=%d tok, cap=%d tok = %.0f%% di %d)",
        tool_name, current, expected_tokens, projected, cap_tokens, ratio * 100, window,
    )
    return (
        "[ERROR: chiamata bloccata da predictive context cap]\n"
        f"Il context attuale e' a {current} token ({pct}% del budget {window}). "
        f"Il risultato atteso del tool aggiungerebbe ~{expected_tokens} token "
        f"portandolo oltre il {int(ratio*100)}% (cap={cap_tokens}).\n"
        "Suggerimenti:\n"
        "- Riduci i parametri (es. length piu' piccolo).\n"
        "- Usa estrazione strutturata (nexus_extract_figma_structure, "
        "nexus_extract_pdf_text, nexus_extract_docx_text).\n"
        "- Chiedi all'utente di fornire una versione testuale."
    )


def _langchain_to_anthropic_messages(messages: list[Any]) -> list[dict]:
    """Converte la lista di BaseMessage LangChain in messaggi Anthropic-compatible.

    Gestisce content=str e content=list[dict] (blocchi tool_use/tool_result).
    """
    out: list[dict] = []
    for m in messages:
        role = getattr(m, "type", None)
        if role == "human":
            anth_role = "user"
        elif role == "ai":
            anth_role = "assistant"
        elif role == "tool":
            anth_role = "user"
        else:
            anth_role = "user"
        content = getattr(m, "content", "")
        extra = getattr(m, "additional_kwargs", {}) or {}
        structured = extra.get("anthropic_content")
        if structured is not None:
            out.append({"role": anth_role, "content": structured})
        else:
            out.append({"role": anth_role, "content": content})
    return out


async def executor_node(state: AgentState) -> dict[str, Any]:
    """Chiama il provider LLM. In modalita' agent (tools_json non vuoto) usa
    `generate_agent_turn` e popola `pending_tool_uses` + `stop_reason`.
    Altrimenti usa il percorso legacy `generate_completion_async`.

    Raccoglie anche metriche estese: token (cache), costo, latency, temperatura.
    """
    import datetime

    messages = list(state.get("messages", []))
    intent = state.get("user_intent", "chat")
    behavior_mode = state.get("behavior_mode", "bilanciata")
    token_budget = state.get("token_budget", 400)
    tools_json = state.get("tools_json") or []
    system_text = state.get("system_text") or ""

    # ── Cluster 1: iniezione plan_rationale nel system_text ───────────────────
    # Se il planner ha prodotto un razionale (gated plan_rationale_enabled),
    # lo prependiamo al system_text dell'executor: cosi' chi esegue conosce il
    # "perche'" del piano, i vincoli e le alternative scartate (continuita'
    # semantica planner->executor). Vuoto/flag OFF => nessun effetto.
    if state.get("plan_rationale"):
        _rat = str(state.get("plan_rationale") or "").strip()
        _constraints = state.get("plan_constraints") or []
        _alternatives = state.get("plan_alternatives") or []
        _block = ["<piano_razionale>", _rat]
        if _constraints:
            _block.append("Vincoli/non-goal: " + "; ".join(str(c) for c in _constraints))
        if _alternatives:
            _alts = [
                f"{a.get('option','?')} (scartata: {a.get('rejected_because','?')})"
                for a in _alternatives if isinstance(a, dict)
            ]
            if _alts:
                _block.append("Alternative scartate: " + "; ".join(_alts))
        _block.append("</piano_razionale>")
        system_text = "\n".join(_block) + "\n\n" + system_text
        logger.info("executor_node: plan_rationale iniettato (%d char)", len(_rat))

    # ── PR-C: worker-mode (orchestrator-worker puro) ──────────────────────────
    # Solo nel run PRINCIPALE (subagent_depth 0/None): dentro un worker
    # (subagent_depth >= 1) NON si applica restrizione, altrimenti il worker
    # 'implement' non potrebbe scrivere. Quando attivo + plan_phase_active +
    # subagents_enabled, l'executor diventa un ORCHESTRATORE: usa il prompt
    # agent.orchestrator.base e filtra i tool sulla whitelist (read-only +
    # delega), forzando la delega ai worker economici.
    _subagent_depth = int(state.get("subagent_depth") or 0)
    if _subagent_depth == 0 and state.get("plan_phase_active"):
        try:
            # orchestrator_config e prompt_registry sono gia' importati a
            # module-level (riga 21-29). Un import locale qui dentro
            # trasformerebbe le variabili in `local` per tutta la funzione
            # `executor_node`, causando UnboundLocalError quando il flusso
            # NON entra in questo branch (es. sub-agent con _subagent_depth>=1)
            # ma poi usa orchestrator_config piu' avanti (vedi Comp.3b).
            if (
                orchestrator_config.worker_mode_enabled()
                and orchestrator_config.subagents_enabled()
            ):
                orch_prompt = prompt_registry.get_prompt("agent.orchestrator.base")
                if orch_prompt:
                    system_text = orch_prompt
                    whitelist = set(orchestrator_config.worker_mode_tool_whitelist())
                    if whitelist and tools_json:
                        filtered = [
                            t for t in tools_json
                            if (t.get("name") if isinstance(t, dict) else getattr(t, "name", None)) in whitelist
                        ]
                        # Non azzerare i tool: se il filtro svuota tutto (es.
                        # whitelist disallineata), lascia i tool originali per
                        # non bloccare il run.
                        if filtered:
                            tools_json = filtered
                    logger.info(
                        "executor_node: worker-mode attivo (orchestratore), tools=%d",
                        len(tools_json),
                    )
        except Exception as exc:
            logger.debug("executor_node: worker-mode skip (%s)", exc)

    # ── Comp.3b: DAG scheduler parallelo (opt-in, mutuamente esclusivo) ────────
    # Forma strutturata del worker-mode: se il piano ha dipendenze (depends_on)
    # e dag_parallel_enabled e' ON, esegue i todo "ready" in parallelo a ondate
    # via dispatch_subagents. Tutto il parallelismo e' confinato in QUESTA
    # invocazione del nodo (loop interno), niente fan-out LangGraph: lo state
    # resta serializzabile. Early-return: salta la chiamata LLM dell'executor.
    _dag_cfg = orchestrator_config.get()
    if (
        _subagent_depth == 0
        and state.get("plan_phase_active")
        and bool(_dag_cfg.get("dag_parallel_enabled"))
    ):
        try:
            from . import dag_scheduler, todo_store as _ts
            _run_id = state.get("thread_id")
            _todos_now = _ts.list_todos(_run_id) if _run_id else []
            _has_deps = any(t.get("depends_on") for t in _todos_now)
            if _has_deps and dag_scheduler.compute_ready_layer(_todos_now):
                total_done = 0
                waves = 0
                # Safety cap sul numero di ondate: non puo' superare il numero
                # di todo (ogni ondata ne completa almeno uno, o si ferma).
                max_waves = len(_todos_now) + 1
                while waves < max_waves:
                    updates = await dag_scheduler.run_dag_layer(state, _tool_runner, _dag_cfg)
                    waves += 1
                    if not updates or not updates.get("active_todo_ids"):
                        break
                    total_done += int(updates.get("completed") or 0)
                    if int(updates.get("completed") or 0) == 0:
                        # Ondata senza completamenti (tutti falliti): stop per
                        # non ciclare a vuoto; i discendenti sono gia' skipped.
                        break
                logger.info(
                    "executor_node: DAG parallelo eseguito (%d ondate, %d todo completati, run_id=%s)",
                    waves, total_done, _run_id,
                )
                # end_turn: route_after_executor -> verifier (se ON, trova tutti
                # terminali e chiude) oppure -> learner. Niente chiamata LLM.
                return {
                    "stop_reason": "end_turn",
                    "pending_tool_uses": [],
                    "iterations": iterations,
                }
        except Exception as exc:
            logger.warning("executor_node: DAG scheduler skip (%s)", exc)

    # ── G1 cap: max re-execution per "risposta descrittiva su action request" ─
    # Se route_after_executor ci ha gia' re-mandato qui >= max_nudges volte
    # senza produrre tool call, fermiamo l'esecuzione con un messaggio
    # assistant esplicito (non si tenta neppure la chiamata al modello).
    # Questo evita loop infiniti quando il modello non rispetta il nudge G1
    # (anche perche' il nudge stesso puo' non essere iniettato, es. la
    # history contiene gia' tool call -> _has_tool_calls_in_history filtra).
    _g1_reroute_count = int(state.get("g1_reroute_count") or 0)
    _g1_max_nudges = _load_g1_max_nudges()
    # Rileva se questa entry e' una re-execution G1: il turno precedente
    # ha chiuso senza tool call (end_turn/stop/None) e abbiamo gia' almeno
    # un'iterazione alle spalle. Verifica anche action_request sul primo
    # messaggio umano per evitare di contare entry non-G1.
    _prev_stop_for_g1 = state.get("stop_reason")
    _prev_iterations_for_g1 = int(state.get("iterations") or 0)
    _is_g1_reentry = False
    if (
        _prev_iterations_for_g1 >= 1
        and _prev_stop_for_g1 in ("end_turn", "stop", None)
        and not (state.get("pending_tool_uses") or [])
    ):
        _first_human_for_g1 = next(
            (getattr(m, "content", "") for m in messages if hasattr(m, "type") and m.type == "human"),
            "",
        )
        if isinstance(_first_human_for_g1, list):
            _first_human_for_g1 = " ".join(
                b.get("text", "") for b in _first_human_for_g1 if isinstance(b, dict)
            )
        if _detect_action_request(str(_first_human_for_g1)):
            # FIX G1 intelligente: se il modello sta legittimamente reagendo a
            # un tool_result d'errore recente (es. "npm install" fallito), NON
            # e' "descrittivo" ma "in difficolta'": dargli piu' tentativi prima
            # del cap. Bug osservato 30/05/2026: gemini-2.5-flash in loop su
            # rebrand shadcn-ui -> shadcn ha esaurito 3 reroute G1 in 7 iter,
            # nonostante stesse facendo 30+ tool call. Soluzione: contare il
            # reroute G1 SOLO se gli ultimi tool result non avevano errori, e
            # consentire una soft-extension (+50% max_nudges) se errori reali.
            _last_tool_was_error = _detect_recent_tool_error(messages, lookback=4)
            if _last_tool_was_error:
                logger.info(
                    "executor_node: re-entry G1 skip-count (tool_result d'errore recente, "
                    "il modello sta reagendo, non descrivendo) reroute_count=%d/%d",
                    _g1_reroute_count, _g1_max_nudges,
                )
            else:
                _is_g1_reentry = True
                _g1_reroute_count += 1
                logger.info(
                    "executor_node: re-entry G1 rilevata, reroute_count=%d/%d",
                    _g1_reroute_count, _g1_max_nudges,
                )
    if _g1_reroute_count >= _g1_max_nudges:
        logger.warning(
            "executor_node: G1 cap raggiunto (reroute_count=%d >= max=%d), "
            "interrompo esecuzione con messaggio assistant esplicito",
            _g1_reroute_count, _g1_max_nudges,
        )
        _cap_text = (
            f"Modello non risponde con azione dopo {_g1_max_nudges} tentativi, "
            f"fermo l'esecuzione. Riformula la richiesta in modo piu' specifico "
            f"oppure prova con un modello piu' capace."
        )
        return {
            "messages": [AIMessage(content=_cap_text)],
            "result": _cap_text,
            "pending_tool_uses": [],
            "stop_reason": "g1_cap_reached",
            "iterations": int(state.get("iterations") or 0) + 1,
            "g1_reroute_count": _g1_reroute_count,
            "action_nudge_count": int(state.get("action_nudge_count") or 0),
        }

    # ── Loop-detection semantica: esplorazione allegati senza scrittura ───────
    # Il contatore accumulato dai turni precedenti conta le chiamate a tool di
    # SOLA esplorazione (lettura/ispezione allegati e file). Se il modello varia
    # entry/offset la loop-detection per signature identica non scatta mai e si
    # arriva a 50+ iterazioni esplorative. Qui agiamo sulla FAMIGLIA di tool:
    #   - >= soglia: iniettiamo UN nudge forte verso la scrittura (una sola volta)
    #   - >= 2x soglia: abortiamo (il modello ha ignorato il nudge)
    # La soglia e' DB-driven (agent.exploration_loop_threshold, default 6).
    _exploration_count = int(state.get("consecutive_exploration_calls") or 0)
    _exploration_threshold = _load_exploration_loop_threshold()
    _exploration_nudge_sent = bool(state.get("exploration_nudge_sent") or False)
    _exploration_nudge_injected = False
    if _exploration_count >= 2 * _exploration_threshold:
        logger.warning(
            "executor_node: LOOP esplorativo (%d chiamate >= 2x soglia %d) senza "
            "scrittura. Abort.",
            _exploration_count, _exploration_threshold,
        )
        _expl_text = (
            f"[LOOP RILEVATO] Il modello ha eseguito {_exploration_count} esplorazioni "
            f"consecutive dell'allegato/file senza scrivere nulla, ignorando il "
            f"sollecito a procedere. Esecuzione interrotta per evitare stallo. "
            f"La specifica e' gia' disponibile nel contesto: riformula la richiesta "
            f"o usa un modello piu' capace per lo scaffolding."
        )
        return {
            "messages": [AIMessage(content=_expl_text)],
            "result": _expl_text,
            "pending_tool_uses": [],
            "stop_reason": "loop_detected",
            "iterations": int(state.get("iterations") or 0) + 1,
            "consecutive_exploration_calls": _exploration_count,
            "exploration_nudge_sent": _exploration_nudge_sent,
        }
    if _exploration_count >= _exploration_threshold and not _exploration_nudge_sent:
        _expl_nudge = HumanMessage(
            content=(
                f"Hai gia' raccolto sufficiente contesto dall'allegato "
                f"({_exploration_count} esplorazioni). La specifica e' gia' "
                f"disponibile nel contesto. NON esplorare ulteriormente "
                f"l'archivio/i file: procedi ORA a creare i file dell'applicazione "
                f"con write_file e richiedi le porte con request_port."
            )
        )
        messages = list(messages) + [_expl_nudge]
        _exploration_nudge_sent = True
        _exploration_nudge_injected = True
        logger.info(
            "executor_node: nudge anti-esplorazione iniettato (count=%d, soglia=%d)",
            _exploration_count, _exploration_threshold,
        )

    # ── Loop-detection comando ripetuto con errori (Fix 30/05/2026) ──────────
    # Diverso da loop-detection esplorativa: qui rileviamo il pattern "stesso
    # comando di esecuzione (run_command/run_service) ripetuto N volte con
    # tool_result d'errore". Caso reale: gemini-2.5-flash su build npm/shadcn
    # ha chiamato `npm run build` 5 volte consecutive, ognuna fallita per
    # dipendenze mancanti, senza cambiare strategia. Senza intervento esaurisce
    # il budget iter. Soglia hardcoded 3 (basso ma sensato: 3 stessi fallimenti
    # = serve cambiare approccio, non riprovare).
    _repeat_cmd, _repeat_count = _detect_repeated_failed_command(messages, lookback=12)
    _repeated_cmd_nudge_sent = bool(state.get("repeated_cmd_nudge_sent") or False)
    if _repeat_cmd and _repeat_count >= 3 and not _repeated_cmd_nudge_sent:
        _cmd_text = (
            f"[LOOP RILEVATO] Hai eseguito `{_repeat_cmd[:120]}` "
            f"{_repeat_count} volte consecutive con errore. Continuare a "
            f"ripetere lo stesso comando non risolvera' il problema. "
            f"CAMBIA STRATEGIA ORA: esamina l'output dell'errore, identifica "
            f"la causa radice (dipendenza mancante? package rinominato? config "
            f"errata?), e prova un approccio diverso (es. tool diverso, comando "
            f"alternativo, lettura della doc, oppure chiedi all'utente)."
        )
        _cmd_nudge = HumanMessage(content=_cmd_text)
        messages = list(messages) + [_cmd_nudge]
        _repeated_cmd_nudge_sent = True
        logger.warning(
            "executor_node: nudge anti-loop-comando iniettato (cmd='%s', count=%d)",
            _repeat_cmd[:80], _repeat_count,
        )

    # ── Forced text response (anti-loop tool-only) ────────────────────────
    # Se il loop ha gia' consumato la maggior parte delle iterazioni concesse
    # (>= MAX_AGENT_ITERATIONS - 5) e il modello sta ancora facendo tool calls
    # (stop_reason precedente era "tool_use"), svuotiamo temporaneamente i tool
    # per forzare una risposta testuale nell'ultima finestra di iterazioni.
    # Questo previene loop in cui modelli small (es. Mistral) fanno tool calls
    # continue senza mai produrre testo, consumando tutte le iterazioni senza
    # lasciare una risposta utile all'utente.
    _current_iterations = int(state.get("iterations") or 0)
    # Mig 0181: soglia forced-text proporzionale al budget adattivo del run.
    _iter_cap = int(state.get("iteration_budget") or 0) or MAX_AGENT_ITERATIONS
    _FORCED_TEXT_THRESHOLD = _iter_cap - 5
    _prev_stop_reason = state.get("stop_reason")
    if (
        tools_json
        and _current_iterations >= _FORCED_TEXT_THRESHOLD
        and _prev_stop_reason == "tool_use"
    ):
        logger.warning(
            "executor_node: forced text response — rimozione tool per produrre risposta "
            "testuale (iterations=%d >= threshold=%d, prev_stop=%s)",
            _current_iterations, _FORCED_TEXT_THRESHOLD, _prev_stop_reason,
        )
        tools_json = []

    # Fix M61 (sticky cascade): se il turno precedente ha fatto cascade fallback
    # con successo a un altro provider, "sticky" su quello nelle iter successive
    # invece di ripartire dal provider primario fallito. Lo sticky ha priorita'
    # ANCHE sul provider_override (l'utente sceglie un primario ma se quel
    # primario fallisce, il cascade-fallback diventa la nuova fonte di verita').
    sticky_provider = state.get("sticky_provider")
    sticky_model = state.get("sticky_model")
    provider = sticky_provider or state.get("provider_override")
    model = sticky_model or state.get("model_override")
    if not provider or not model:
        if _router is not None:
            # Passa anche il message originale: il router lo usa per detection
            # task rischiosi (override automatico a behavior_mode "approfondita"
            # se rileva verbi distruttivi: rm -rf, drop table, docker prune, ecc.)
            _last_msg_text = messages[-1].content if messages else ""
            decision = _router.route_model(intent, token_budget, behavior_mode, message=str(_last_msg_text))
            provider = provider or decision.provider
            model = model or decision.model
            logger.info("executor_node routing: %s", decision.rationale)
        else:
            # Router non disponibile: risolvi da DB (nexus_purpose_model).
            # Niente fallback hardcoded.
            try:
                from brain.router.service import _routing_client_singleton
                decision = _routing_client_singleton().purpose_model(purpose="agent_tier_sonnet")
                provider = provider or decision.provider
                model = model or decision.model
            except Exception as e:
                logger.error("executor_node: impossibile risolvere modello da DB: %s", e)
                raise RuntimeError(
                    "Nessun router disponibile e nexus_purpose_model non raggiungibile. "
                    "Verifica che mcp-core sia attivo e la migrazione 0102 sia applicata."
                ) from e

    logger.info(
        "executor_node: provider=%s model=%s intent=%s tools=%d",
        provider, model, intent, len(tools_json),
    )
    # Nexus thinking: lista locale, popolata lungo l'executor; emessa nel
    # return finale (backward-compat / final state) E in tempo reale via
    # _stream_thinking_live (custom stream LangGraph) cosi' l'utente vede i
    # ragionamenti SCORRERE durante l'attesa, non tutti insieme a fine nodo.
    _executor_thinking: list[str] = []
    _thinking_on = _nexus_thinking_enabled()

    def _think(line: str) -> None:
        """Accoda una riga di thinking E la emette live sul custom stream.

        Unico punto di emissione thinking dell'executor: garantisce che ogni
        ragionamento esca immediatamente (live) e resti anche nel return finale
        per il delta di fine nodo (consumer SSE storico).
        """
        if not line:
            return
        txt = str(line).strip()
        if not txt:
            return
        _executor_thinking.append(txt)
        if _thinking_on:
            _stream_thinking_live(txt)

    _think(
        f"Routing modello: {provider}/{model} (intent {intent}, tools disponibili {len(tools_json)})."
    )
    # FIX 2: ragionamento d'ingresso prima della chiamata LLM, emesso live.
    _think(
        f"Iterazione {_current_iterations}: consulto il modello {provider}/{model}..."
    )

    # ── G1 Python: nudge anti-descrittivo ─────────────────────────────────────
    # Se il modello ha già risposto almeno una volta (iterations >= 1) senza
    # chiamare NESSUN tool (risposta puramente descrittiva) e la richiesta
    # originale era un'azione concreta (avvia, installa, crea, docker, ...)
    # inietta un messaggio di nudge PRIMA di chiamare il LLM.
    # Cap: max 2 nudge per run (action_nudge_count in state) per evitare loop.
    _current_iter = int(state.get("iterations") or 0)
    _nudge_count = int(state.get("action_nudge_count") or 0)
    if (
        tools_json                                        # l'agente ha tool disponibili
        and _current_iter >= 1                            # route_after_executor ha già ri-mandato qui
        and _nudge_count < 2                              # max 2 nudge totali
        and not _has_tool_calls_in_history(messages)     # nessun tool call nella history
    ):
        # Estrai il primo messaggio umano per capire se è action-oriented
        _first_human_text = next(
            (getattr(m, "content", "") for m in messages if hasattr(m, "type") and m.type == "human"),
            "",
        )
        if isinstance(_first_human_text, list):
            _first_human_text = " ".join(
                b.get("text", "") for b in _first_human_text if isinstance(b, dict)
            )
        if _detect_action_request(str(_first_human_text)):
            _nudge_msg = HumanMessage(
                content=(
                    "⚠️ ERRORE: hai risposto descrivendo cosa avresti fatto, "
                    "ma NON hai chiamato nessun tool. Questo non è accettabile. "
                    "AGISCI ADESSO — usa shell_exec per comandi (docker, npm, dotnet, ecc.), "
                    "write_file/edit_file per creare o modificare file. "
                    "Nessuna spiegazione: ESEGUI il prossimo step concreto con un tool call."
                )
            )
            messages = list(messages) + [_nudge_msg]
            logger.warning(
                "G1 nudge iniettato (iter=%d, nudge_count=%d, intent=%s)",
                _current_iter, _nudge_count, intent,
            )

    # Heartbeat A: pubblica step "calling_model" cosi' l'utente vede in chat
    # che l'agente sta lavorando (LLM call puo' durare 30-60s su contesti grandi).
    try:
        _calling_meta = meta_steps.make(
            kind="executor_call",
            title=f"Sto interrogando {provider}/{model}",
            payload={
                "provider": provider,
                "model": model,
                "intent": intent,
                "iteration": state.get("iterations", 0),
                "tools_count": len(tools_json),
            },
        )
        if _calling_meta:
            meta_steps.persist_async(state.get("thread_id"), _calling_meta)
    except Exception as _hb_exc:
        logger.debug("executor_node: heartbeat calling_model fallito: %s", _hb_exc)

    start_ms = time.monotonic() * 1000
    created_at = datetime.datetime.now(datetime.timezone.utc).isoformat()
    result_text = ""
    token_usage = 0
    prompt_tokens = 0
    completion_tokens = 0
    cache_creation_tokens = 0
    cache_read_tokens = 0
    total_cost_usd = 0.0
    temperature = 0.2  # default Anthropic
    top_p = 0.9  # default
    pending_tool_uses: list[dict] = []
    stop_reason: str | None = "end_turn"
    assistant_msg: AIMessage | None = None

    if _providers is None:
        result_text = "[Servizi non configurati]"
    elif tools_json:
        ctx_size = _estimate_context_chars(messages)
        # ── Rolling summarization (BP4) ──────────────────────────────────────
        # Se siamo oltre il 60% di MAX_CONTEXT_CHARS, prima di troncare
        # tool_result tentiamo di riassumere i messaggi vecchi con un modello
        # small. Cosi' preserviamo le decisioni e gli errori passati invece
        # di perderli con il truncation. Best-effort: se fallisce, fallback
        # alla compressione standard.
        from brain.agents import summarizer as _summarizer
        if _summarizer.should_trigger_summary(ctx_size, MAX_CONTEXT_CHARS):
            try:
                summarized = await _summarizer.summarize_old_messages(
                    messages,
                    providers=_providers,
                    keep_recent=_summarizer.DEFAULT_KEEP_RECENT,
                    thread_id=str(state.get("thread_id") or ""),
                )
                if summarized is not None:
                    messages = summarized
                    new_size = _estimate_context_chars(messages)
                    logger.info(
                        "executor_node: rolling summary %d -> %d char",
                        ctx_size, new_size,
                    )
                    ctx_size = new_size
            except Exception as exc:
                logger.warning("executor_node: summarizer fallito: %s", exc)
        # ── FIX A-C (ADR 0014): context size management ──────────────────────
        # Pipeline ordinata:
        #   1) FIX B: dedup tool_result identici per signature (tool_name+args).
        #   2) FIX C: drop body base64 vecchi non citati dalla history.
        #   3) FIX A: compressione escalante anticipata da iter compress_start_iter
        #      (default 5). I parametri keep_recent / max_content_chars sono
        #      DB-driven (mig 0199).
        # Tutto in cache 60s via _load_ctx_mgmt_config().
        _ctx_cfg = _load_ctx_mgmt_config()
        _compress_iter = _current_iterations

        # FIX B: dedup tool_result identici per signature.
        if _ctx_cfg.get("dedup_tool_results_enabled", True):
            _pre_dedup_size = ctx_size
            messages = _dedup_tool_results_history(messages)
            _post_dedup = _estimate_context_chars(messages)
            if _post_dedup != _pre_dedup_size:
                logger.info(
                    "executor_node: FIX B dedup signature iter=%d: %d -> %d char",
                    _compress_iter, _pre_dedup_size, _post_dedup,
                )
                ctx_size = _post_dedup

        # FIX C: drop base64 vecchi non citati.
        _pre_drop = ctx_size
        messages = _drop_unused_base64_payloads(messages)
        _post_drop = _estimate_context_chars(messages)
        if _post_drop != _pre_drop:
            logger.info(
                "executor_node: FIX C drop base64 iter=%d: %d -> %d char",
                _compress_iter, _pre_drop, _post_drop,
            )
            ctx_size = _post_drop

        # FIX A: compressione escalante anticipata.
        _compress, _params = _should_compress_now(_compress_iter, _ctx_cfg)
        if _compress:
            messages = _compress_old_tool_results(
                messages,
                keep_recent=_params["keep_recent"],
                max_content_chars=_params["max_content_chars"],
            )
            new_size = _estimate_context_chars(messages)
            logger.info(
                "executor_node: FIX A compressione iter=%d keep_recent=%d "
                "max_content_chars=%d: %d -> %d char",
                _compress_iter, _params["keep_recent"], _params["max_content_chars"],
                ctx_size, new_size,
            )
            ctx_size = new_size
        elif ctx_size > MAX_CONTEXT_CHARS // 2:
            # Safety net storica: contesto >50% MAX anche pre-iter=start.
            messages = _compress_old_tool_results(messages, keep_recent=6)
            new_size = _estimate_context_chars(messages)
            logger.info(
                "executor_node: contesto compresso (safety net) %d -> %d char",
                ctx_size, new_size,
            )
        # Bug #88: reminder lingua resiliente al contesto/profilo. Iniettato qui
        # cosi' copre sia la chiamata principale (generate_agent_turn_sync con
        # system_text) sia il fallback anti-loop (system_text2 = system_text +
        # anti_loop_hint, che eredita il reminder dal system_text base). Doppia
        # iniezione: garanzia nel system + recency sull'ultimo HumanMessage.
        _lang_enabled, _lang_text = _load_language_reminder()
        messages, system_text = _inject_language_reminder(
            messages, system_text, _lang_enabled, _lang_text
        )
        anth_messages = _langchain_to_anthropic_messages(messages)
        try:
            # max_tokens dinamico: almeno 8192 per turni con tool, capped a 16384.
            # Il token_budget dallo state (stimato dal router_node) viene usato
            # come base, ma per agent turn con tool serve molto piu' spazio
            # (tool_use JSON e' verboso, content_json per documenti puo' essere enorme).
            effective_max_tokens = max(8192, min(token_budget * 4, 16384))
            # Fix M60: passa dal registry per riusare la cascade fallback esistente
            # (provider_hierarchy + nexus_provider_default_model + classify_error).
            # generate_agent_turn_sync e' bloccante, lo runno in thread.
            prov_result = await asyncio.to_thread(
                _providers.generate_agent_turn_sync,
                provider, model, anth_messages, tools_json,
                max_tokens=effective_max_tokens, system_text=system_text,
            )
            # Aggiorna provider/model effettivamente usati se la cascade ha fatto fallback.
            # Salva anche come "sticky" per le iter successive (M61): evita di ri-tentare
            # il provider primario fallito ad ogni round, risparmiando latenza/cost.
            cascade_did_fallback = False
            if prov_result.provider and prov_result.provider != provider:
                logger.info(
                    "executor_node: cascade fallback %s -> %s/%s (sticky per iter successive)",
                    provider, prov_result.provider, prov_result.model,
                )
                provider = prov_result.provider
                model = prov_result.model
                cascade_did_fallback = True
                # Fix M62: propaga il provider/model effettivi anche su agent_runs cosi'
                # la chat UI mostra il fallback invece del primario fallito. Best-effort:
                # se il DB e' down logghiamo soltanto.
                try:
                    import os, psycopg2  # type: ignore[import-untyped]
                    _run_id = str(state.get("thread_id") or "")
                    _dburl = os.environ.get("DATABASE_URL", "")
                    if _run_id and _dburl:
                        _conn = psycopg2.connect(_dburl)
                        with _conn.cursor() as _cur:
                            _cur.execute(
                                "UPDATE agent_runs SET provider=%s, model=%s WHERE id=%s",
                                (provider, model, _run_id),
                            )
                        _conn.commit()
                        _conn.close()
                except Exception as _exc:
                    logger.warning("executor_node: UPDATE agent_runs cascade fallita: %s", _exc)
            result_text = prov_result.content or ""
            meta = prov_result.metadata or {}
            stop_reason = meta.get("stop_reason") or "end_turn"
            pending_tool_uses = list(meta.get("tool_use_blocks") or [])
            assistant_content = meta.get("assistant_content")
            # Heartbeat A: pubblica step "tool_calls" con elenco tool richiesti.
            if pending_tool_uses:
                try:
                    _tools_meta = meta_steps.make(
                        kind="tool_calls",
                        title=f"Sto usando {len(pending_tool_uses)} tool",
                        payload={
                            "tools": [
                                {
                                    "name": tu.get("name"),
                                    "input_keys": list((tu.get("input") or {}).keys())[:8],
                                }
                                for tu in pending_tool_uses[:10]
                            ],
                            "iteration": _current_iterations,
                        },
                    )
                    if _tools_meta:
                        meta_steps.persist_async(state.get("thread_id"), _tools_meta)
                except Exception as _hb_exc:
                    logger.debug("executor_node: heartbeat tool_calls fallito: %s", _hb_exc)
            usage = meta.get("usage") or {}
            prompt_tokens = int(usage.get("input_tokens", 0))
            completion_tokens = int(usage.get("output_tokens", 0))
            cache_creation_tokens = int(usage.get("cache_creation_input_tokens", 0))
            cache_read_tokens = int(usage.get("cache_read_input_tokens", 0))
            token_usage = prompt_tokens + completion_tokens

            input_price, output_price = _lookup_price(provider, model)
            if input_price > 0 or output_price > 0:
                cache_write_price = input_price * 1.25
                cache_read_price = input_price * 0.1
                total_cost_usd = (
                    (prompt_tokens * input_price) / 1_000_000.0 +
                    (completion_tokens * output_price) / 1_000_000.0 +
                    (cache_creation_tokens * cache_write_price) / 1_000_000.0 +
                    (cache_read_tokens * cache_read_price) / 1_000_000.0
                )

            # Preserviamo il content strutturato per il prossimo round.
            assistant_msg = AIMessage(
                content=result_text,
                additional_kwargs={"anthropic_content": assistant_content} if assistant_content else {},
            )

            # Registra l'usage di questo turno nel billing ledger (B1 fix).
            try:
                from brain.providers.registry import record_agent_turn_usage
                record_agent_turn_usage(
                    provider=provider,
                    model=model,
                    prompt_tokens=prompt_tokens,
                    completion_tokens=completion_tokens,
                    cache_read_tokens=cache_read_tokens,
                    cache_creation_tokens=cache_creation_tokens,
                    iteration=_current_iterations,
                    run_id=str(state.get("thread_id") or ""),
                )
            except Exception as billing_exc:
                logger.warning("executor_node: billing ledger fallito iter=%d: %s", _current_iterations, billing_exc)

        except Exception as exc:
            logger.error("executor_node: agent_turn %s/%s: %s", provider, model, exc)
            result_text = f"[Errore provider {provider}: {exc}]"
            stop_reason = "error"
    else:
        # Percorso legacy single-shot.
        last_text = ""
        if messages:
            last = messages[-1]
            last_text = last.content if hasattr(last, "content") else str(last)
        try:
            prov_result = await _providers.generate_completion_async(
                provider, model, last_text
            )
            result_text = prov_result.content
            usage = prov_result.metadata.get("usage", {})
            prompt_tokens = int(usage.get("input_tokens", 0))
            completion_tokens = int(usage.get("output_tokens", 0))
            cache_read_tokens = int(usage.get("cache_read_input_tokens", 0))
            token_usage = prompt_tokens + completion_tokens

            input_price, output_price = _lookup_price(provider, model)
            if input_price > 0 or output_price > 0:
                cache_read_price = input_price * 0.1
                total_cost_usd = (
                    (prompt_tokens * input_price) / 1_000_000.0 +
                    (completion_tokens * output_price) / 1_000_000.0 +
                    (cache_read_tokens * cache_read_price) / 1_000_000.0
                )
        except Exception as exc:
            logger.error("executor_node: completion %s/%s: %s", provider, model, exc)
            result_text = f"[Errore provider {provider}: {exc}]"

    if assistant_msg is None:
        assistant_msg = AIMessage(content=result_text)

    latency_ms = time.monotonic() * 1000 - start_ms

    # ── Loop detection ────────────────────────────────────────────────────
    # Se il modello ripete la stessa tool call (stesso name + stesso input)
    # 3 volte di fila, abortiamo con messaggio chiaro: il modello e' bloccato.
    new_signatures: list[str] = []
    for tu in pending_tool_uses:
        sig_input = json.dumps(tu.get("input") or {}, sort_keys=True, ensure_ascii=False)
        sig = f"{tu.get('name', '')}|{hashlib.sha1(sig_input.encode()).hexdigest()[:12]}"
        new_signatures.append(sig)
    recent: list[str] = list(state.get("recent_tool_signatures") or [])
    combined = recent + new_signatures
    LOOP_THRESHOLD = 3
    loop_sig: str | None = None
    if len(combined) >= LOOP_THRESHOLD and new_signatures:
        for sig in new_signatures:
            tail = [s for s in combined[-LOOP_THRESHOLD * 2:] if s == sig]
            if len(tail) >= LOOP_THRESHOLD:
                loop_sig = sig
                break
    if loop_sig is not None:
        tool_name = loop_sig.split("|", 1)[0]
        logger.warning(
            "executor_node: LOOP detected %s/%s ripete tool='%s' (signature=%s) %d+ volte. Abort.",
            provider, model, tool_name, loop_sig, LOOP_THRESHOLD,
        )
        # Auto-escalation: al primo loop proviamo automaticamente un modello più capace.
        # Priorità:
        #   1. Catena intra-provider (nexus_model_escalation_chain) — stesso provider, tier superiore
        #   2. Purpose model DB-driven dal router Rust (loop_fallback_default)
        #   3. Nessun fallback hardcoded: se nulla disponibile, segna loop_detected
        escalations = int(state.get("auto_escalations") or 0)
        tried_escalation = False
        if escalations < 3 and _providers is not None and tools_json:
            try:
                fallback_provider: str | None = None
                fallback_model: str | None = None

                # === Tier 1: catena intra-provider ===
                try:
                    import psycopg2  # type: ignore[import]
                    import os as _os
                    _db_url = _os.environ.get(
                        "DATABASE_URL",
                        "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable",
                    )
                    with psycopg2.connect(_db_url) as _conn:
                        with _conn.cursor() as _cur:
                            _cur.execute(
                                "SELECT escalation_model FROM nexus_model_escalation_chain "
                                "WHERE provider = %s AND base_model = %s AND is_active = TRUE "
                                "ORDER BY escalation_position ASC LIMIT %s",
                                (provider, model, escalations + 1),
                            )
                            _rows = _cur.fetchall()
                    if _rows and len(_rows) > escalations:
                        # Prende la posizione corrispondente al numero di escalation già fatte
                        candidate_model = _rows[escalations][0]
                        if _providers._providers.get(provider):  # type: ignore[attr-defined]
                            fallback_provider = provider
                            fallback_model = candidate_model
                            logger.info(
                                "executor_node: LOOP catena intra-provider pos=%d %s/%s => %s/%s",
                                escalations + 1, provider, model, provider, candidate_model,
                            )
                except Exception as _e:
                    logger.debug("executor_node: catena escalation DB fallita: %s", _e)

                # === Tier 2: purpose model dal router Rust (cross-provider) ===
                if not fallback_provider:
                    try:
                        from brain.router.service import _routing_client_singleton
                        decision2 = _routing_client_singleton().purpose_model(purpose="loop_fallback_default")
                        if decision2.provider not in ("__router_unavailable__", "__no_capable_provider__"):
                            fallback_provider = decision2.provider
                            fallback_model = decision2.model
                            logger.info(
                                "executor_node: LOOP -> purpose_model fallback %s/%s => %s/%s",
                                provider, model, fallback_provider, fallback_model,
                            )
                    except Exception:
                        pass

                if fallback_provider and fallback_model:
                    p2 = _providers._providers.get(fallback_provider)  # type: ignore[attr-defined]
                    if p2 is not None and hasattr(p2, "generate_agent_turn"):
                        # Hint anti-loop nel system_text: chiede esplicitamente di NON ripetere il tool.
                        anti_loop_hint = (
                            "\n\n[ANTI-LOOP] Hai appena ripetuto la stessa tool call "
                            f"('{tool_name}' con stesso input) più volte. "
                            "Non ripetere la stessa tool call con lo stesso input. "
                            "Se mancano informazioni, fai UNA richiesta più specifica "
                            "oppure cambia strategia e riassumi lo stato."
                        )
                        system_text2 = (system_text or "") + anti_loop_hint

                        # Riprova lo stesso turno agente con provider/model più capace.
                        prov2 = await p2.generate_agent_turn(
                            fallback_model,
                            anth_messages,
                            tools_json,
                            max_tokens=effective_max_tokens,
                            system_text=system_text2,
                        )
                        result_text = prov2.content or ""
                        meta2 = prov2.metadata or {}
                        stop_reason = meta2.get("stop_reason") or "end_turn"
                        pending_tool_uses = list(meta2.get("tool_use_blocks") or [])
                        assistant_content2 = meta2.get("assistant_content")
                        if assistant_content2:
                            assistant_msg = AIMessage(
                                content="",
                                additional_kwargs={"anthropic_content": assistant_content2},
                            )
                        else:
                            assistant_msg = AIMessage(content=result_text)

                        provider = fallback_provider
                        model = fallback_model
                        tried_escalation = True
                        new_signatures = []  # reset signature accumulator dopo escalation
            except Exception as exc:
                logger.warning("executor_node: auto-escalation fallita: %s", exc)

        if not tried_escalation:
            loop_msg = (
                f"[LOOP RILEVATO] Il modello {provider}/{model} ha ripetuto il tool "
                f"'{tool_name}' con stesso input {LOOP_THRESHOLD}+ volte senza progresso. "
                f"Esecuzione interrotta per evitare stallo. "
                f"Suggerimento: usa un modello piu' capace (es. anthropic/claude) o "
                f"riformula il prompt in modo piu' specifico."
            )
            assistant_msg = AIMessage(content=loop_msg)
            pending_tool_uses = []
            stop_reason = "loop_detected"
            result_text = loop_msg
            new_signatures = []  # reset, non accumulare ulteriormente
        else:
            # Persisti lo stato di escalation per evitare retry infiniti.
            escalations += 1

    # Mantieni solo le ultime ~12 signature per non gonfiare lo state.
    updated_signatures = (recent + new_signatures)[-12:]

    # ── Aggiorna contatore loop-detection semantica ───────────────────────────
    # Conta le tool call di SOLA esplorazione del turno corrente. Se TUTTE le
    # call pending sono esplorative, accumula (il modello sta ancora leggendo).
    # Se almeno una e' PRODUTTIVA (qualunque tool NON in _EXPLORATION_ONLY_TOOLS,
    # es. write_file/edit_file/run_command/request_port), azzera contatore e
    # flag nudge: il modello ha iniziato a produrre. Senza tool call (risposta
    # testuale) il contatore resta invariato.
    _updated_exploration_count = int(state.get("consecutive_exploration_calls") or 0)
    _updated_exploration_nudge_sent = (
        _exploration_nudge_sent if _exploration_nudge_injected
        else bool(state.get("exploration_nudge_sent") or False)
    )
    if pending_tool_uses:
        _pending_names = [str(tu.get("name", "")) for tu in pending_tool_uses]
        _all_exploration = all(n in _EXPLORATION_ONLY_TOOLS for n in _pending_names)
        if _all_exploration:
            _updated_exploration_count += len(_pending_names)
        else:
            # Almeno una call produttiva: il modello sta scrivendo, reset.
            _updated_exploration_count = 0
            _updated_exploration_nudge_sent = False

    # Calcola cache hit rate
    total_tokens = prompt_tokens + completion_tokens + cache_creation_tokens + cache_read_tokens
    cache_hit_rate = cache_read_tokens / total_tokens if total_tokens > 0 else 0.0

    # M61 sticky cascade: se nel turno corrente c'è stato fallback, salva i
    # provider/model effettivi nello state cosi' le iter successive partono
    # direttamente da li' senza ri-tentare il provider primario fallito.
    sticky_out = {
        "sticky_provider": provider if (locals().get("cascade_did_fallback") or state.get("sticky_provider")) else state.get("sticky_provider"),
        "sticky_model": model if (locals().get("cascade_did_fallback") or state.get("sticky_model")) else state.get("sticky_model"),
    }

    # Nexus thinking: descrivi azioni decise da questo turno, emesse LIVE.
    try:
        # FIX 2: testo intermedio del modello (il ragionamento che accompagna
        # le tool call). Se presente, emettiamo le prime ~200 char come thinking
        # cosi' l'utente vede il "perche'" dietro le azioni mentre scorrono.
        if result_text and pending_tool_uses:
            _intermediate = str(result_text).strip()
            if _intermediate:
                _snippet = _intermediate[:200]
                if len(_intermediate) > 200:
                    _snippet += "..."
                _think(_snippet)
        # FIX 2: per ogni tool, frase italiana leggibile derivata dagli args.
        for _tu in (pending_tool_uses or []):
            if isinstance(_tu, dict):
                _tu_name = _tu.get("name") or "sconosciuto"
                _tu_args = _tu.get("input") if isinstance(_tu.get("input"), dict) else _tu.get("args")
            else:
                _tu_name, _tu_args = "sconosciuto", None
            _think(_describe_tool_call(str(_tu_name), _tu_args))
        if stop_reason == "end_turn" and not pending_tool_uses:
            _think("Risposta finale generata (end_turn).")
    except Exception as _thinking_exc:
        logger.debug("executor_node: thinking append fallito: %s", _thinking_exc)

    _thinking_payload: dict[str, Any] = {}
    if _executor_thinking and _thinking_on:
        _thinking_payload["nexus_thinking"] = list(_executor_thinking)

    return {
        "messages": [assistant_msg],
        "result": result_text,
        "provider_used": provider,
        "model_used": model,
        "latency_ms": latency_ms,
        "token_usage": token_usage or None,
        "feedback_score": None,
        "pending_tool_uses": pending_tool_uses,
        "stop_reason": stop_reason,
        "recent_tool_signatures": updated_signatures,
        "consecutive_exploration_calls": _updated_exploration_count,
        "exploration_nudge_sent": _updated_exploration_nudge_sent,
        "repeated_cmd_nudge_sent": _repeated_cmd_nudge_sent,
        "auto_escalations": escalations if loop_sig is not None else int(state.get("auto_escalations") or 0),
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "cache_creation_tokens": cache_creation_tokens,
        "cache_read_tokens": cache_read_tokens,
        "total_tokens": total_tokens,
        "total_cost_usd": total_cost_usd,
        "cache_hit_rate": cache_hit_rate,
        "temperature": temperature,
        "top_p": top_p,
        "created_at": created_at,
        # Bug-fix: incrementa iterations a ogni passaggio per il nodo executor.
        # Senza questo, il safety net `iterations >= MAX_AGENT_ITERATIONS` in
        # route_after_executor non scatta mai durante i loop
        # executor → tool_dispatch → executor o executor → executor (G1 nudge).
        # Senza l'increment, il G1 nudge poteva loop-are infinite volte con
        # iter=1 nudge=0 quando il modello produceva sempre risposte descrittive.
        "iterations": int(state.get("iterations") or 0) + 1,
        # G1: incrementa il counter nudge se è stato iniettato in questo turno
        "action_nudge_count": (
            _nudge_count + 1
            if (
                locals().get("_nudge_msg") is not None
                and not pending_tool_uses  # il nudge non ha prodotto tool calls ancora
            )
            else _nudge_count
        ),
        # G1: persisti il counter di re-execution G1 (incrementato sopra
        # all'inizio dell'executor se questa entry e' una re-entry G1).
        # Indipendente da action_nudge_count: viene alzato ANCHE quando il
        # nudge non puo' essere iniettato (es. history con tool call),
        # garantendo che il cap configurabile (agent.g1_max_nudges) scatti
        # in ogni scenario ed eviti loop infiniti.
        "g1_reroute_count": _g1_reroute_count,
        **sticky_out,
        **_thinking_payload,
    }


# ─── Nodo: tool_dispatch ─────────────────────────────────────────────────────



# ── FIX 4 (ADR 0012): budget letture allegati ──────────────────────────────
# Legge agent.attachment.session_read_budget_bytes da settings (cache 60s).
# Default 500000 = 500 KB cumulativi per sessione. Tool intercettati:
# nexus_read_attachment, nexus_read_archive_entry.
_ATTACHMENT_BUDGET_CACHE: dict = {"value": None, "loaded_at": 0.0}
_ATTACHMENT_BUDGET_TTL = 60.0

def _attachment_budget_bytes() -> int:
    import os as _os, time as _time
    now = _time.time()
    if _ATTACHMENT_BUDGET_CACHE.get("value") is not None        and now - _ATTACHMENT_BUDGET_CACHE.get("loaded_at", 0.0) < _ATTACHMENT_BUDGET_TTL:
        return _ATTACHMENT_BUDGET_CACHE["value"]
    value = 500_000
    try:
        import psycopg2  # type: ignore[import-untyped]
        dburl = _os.environ.get("DATABASE_URL", "")
        if dburl:
            conn = psycopg2.connect(dburl)
            try:
                with conn.cursor() as cur:
                    cur.execute(
                        "SELECT value FROM settings WHERE key = %s",
                        ("agent.attachment.session_read_budget_bytes",),
                    )
                    row = cur.fetchone()
                    if row and row[0] is not None:
                        try:
                            value = int(str(row[0]).strip().strip('"'))
                        except ValueError:
                            pass
            finally:
                conn.close()
    except Exception as exc:
        logger.debug("_attachment_budget_bytes: lettura DB fallita: %s", exc)
    _ATTACHMENT_BUDGET_CACHE["value"] = value
    _ATTACHMENT_BUDGET_CACHE["loaded_at"] = now
    return value


_ATTACHMENT_READ_TOOLS = {"nexus_read_attachment", "nexus_read_archive_entry"}


def _extract_returned_bytes(result_content: str) -> int:
    """Stima i byte effettivamente letti dal tool_result (campo )."""
    try:
        import json as _json
        data = _json.loads(result_content) if result_content else {}
        if isinstance(data, dict):
            v = data.get("length")
            if isinstance(v, int):
                return max(0, v)
    except Exception:
        pass
    return 0


async def tool_dispatch_node(state: AgentState) -> dict[str, Any]:
    """Esegue i tool_use richiesti dall'LLM tramite ToolRunner gRPC e
    restituisce un HumanMessage con i blocchi `tool_result` corrispondenti.
    """
    pending = list(state.get("pending_tool_uses") or [])
    session_id = state.get("session_id")

    if not pending:
        return {"pending_tool_uses": [], "stop_reason": "end_turn"}

    if _tool_runner is None:
        logger.error("tool_dispatch_node: ToolRunnerClient non configurato")
        error_blocks = [
            {
                "type": "tool_result",
                "tool_use_id": b.get("id", ""),
                "content": json.dumps({"error": "tool_runner_not_configured"}),
                "is_error": True,
            }
            for b in pending
        ]
        tool_msg = HumanMessage(
            content="", additional_kwargs={"anthropic_content": error_blocks},
        )
        return {
            "messages": [tool_msg],
            "pending_tool_uses": [],
            "stop_reason": "tool_use",
        }

    if not session_id:
        logger.error("tool_dispatch_node: session_id assente, impossibile eseguire i tool")
        error_blocks = [
            {
                "type": "tool_result",
                "tool_use_id": b.get("id", ""),
                "content": json.dumps({"error": "missing_session_id"}),
                "is_error": True,
            }
            for b in pending
        ]
        tool_msg = HumanMessage(
            content="", additional_kwargs={"anthropic_content": error_blocks},
        )
        return {
            "messages": [tool_msg],
            "pending_tool_uses": [],
            "stop_reason": "tool_use",
        }

    ctx_chars = _estimate_context_chars(list(state.get("messages") or []))

    # FIX 4 (ADR 0012): budget letture allegati. Se la prossima chiamata a
    # nexus_read_attachment / nexus_read_archive_entry farebbe superare il
    # budget cumulativo della sessione, sostituisco il tool_use con un
    # tool_result sintetico che istruisce il modello a usare i tool di
    # estrazione strutturata.
    current_bytes = int(state.get("attachment_read_bytes") or 0)
    budget_total = _attachment_budget_bytes()
    synthetic_results: list[dict] = []
    pending_kept: list[dict] = []
    pending_kept_indices: list[int] = []
    # FIX D (ADR 0014): predictive context cap. Modello + system_text + history
    # corrente vengono usati per stimare se la chiamata sforerebbe ratio*window.
    _predictive_model = (
        state.get("sticky_model")
        or state.get("model_override")
        or ""
    )
    _predictive_messages = list(state.get("messages") or [])
    _predictive_system = state.get("system_text") or ""
    for _i, b in enumerate(pending):
        name = b.get("name", "")
        # FIX D: predictive cap (ha priorita' sul budget allegati: se passa di qui,
        # poi viene comunque verificato anche il budget).
        _cap_msg = _predictive_cap_check(
            tool_name=str(name or ""),
            args=b.get("input", {}) or {},
            messages=_predictive_messages,
            model=str(_predictive_model or ""),
            system_text=str(_predictive_system or ""),
        )
        if _cap_msg is not None:
            synthetic_results.append({
                "type": "tool_result",
                "tool_use_id": b.get("id", ""),
                "content": _cap_msg,
                "is_error": True,
            })
            continue
        if name in _ATTACHMENT_READ_TOOLS and current_bytes >= budget_total:
            err_payload = {
                "error": (
                    f"budget letture allegati esaurito ({current_bytes} byte gia' "
                    f"letti su {budget_total} budget). Usa un tool di estrazione "
                    f"strutturata (nexus_extract_pdf_text, nexus_extract_figma_structure, "
                    f"nexus_extract_docx_text, nexus_extract_xlsx_data) oppure chiedi "
                    f"all'utente una versione testuale del file."
                ),
                "budget_bytes": budget_total,
                "already_read": current_bytes,
            }
            logger.warning(
                "tool_dispatch_node: budget letture allegati esaurito (%d/%d), tool=%s bloccato",
                current_bytes, budget_total, name,
            )
            synthetic_results.append({
                "type": "tool_result",
                "tool_use_id": b.get("id", ""),
                "content": json.dumps(err_payload, ensure_ascii=False),
                "is_error": True,
            })
        else:
            pending_kept.append(b)
            pending_kept_indices.append(_i)

    async def _run(block: dict) -> dict:
        tool_use_id = block.get("id", "")
        _t_name = block.get("name", "")
        _t_input = block.get("input", {}) or {}
        logger.info(
            "TOOL_CALL tool=%s session=%s input_keys=%s",
            _t_name,
            session_id,
            list(_t_input.keys()) if isinstance(_t_input, dict) else str(_t_input)[:80],
        )
        try:
            result = await _tool_runner.execute_tool(
                tool_name=_t_name,
                tool_input=_t_input,
                session_id=session_id,
                tool_use_id=tool_use_id,
            )
            content = _smart_truncate_lossless(
                result.result_json,
                source_kind="tool_result",
                metadata={
                    "tool_name": _t_name,
                    "session_id": session_id,
                    "thread_id": str(state.get("thread_id") or ""),
                },
            )
            return {
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
                "is_error": bool(result.is_error),
            }
        except Exception as exc:
            logger.exception("tool_dispatch_node: errore tool %s", block.get("name"))
            return {
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": json.dumps({"error": str(exc)}),
                "is_error": True,
            }

    results_kept = await asyncio.gather(*[_run(b) for b in pending_kept]) if pending_kept else []
    # Ricompongo results nell'ordine originale di pending.
    results: list[dict] = []
    _kept_iter = iter(results_kept)
    _synth_iter = iter(synthetic_results)
    for _i, b in enumerate(pending):
        if _i in pending_kept_indices:
            results.append(next(_kept_iter))
        else:
            results.append(next(_synth_iter))

    # Aggiorna byte cumulativi sommando i length dei tool_result attachment_read.
    added_bytes = 0
    for b, r in zip(pending, results):
        if b.get("name", "") in _ATTACHMENT_READ_TOOLS and not r.get("is_error"):
            added_bytes += _extract_returned_bytes(r.get("content", ""))
    new_attachment_read_bytes = current_bytes + added_bytes


    # M68: persisti gli step in agent_steps INCREMENTALMENTE durante il run.
    # Prima il batch finale era in chat_messages.rs:2350 (Rust) a fine run.
    # Ora ogni step viene scritto subito cosi' la UI lo vede via polling
    # (M67) o via refresh, senza dover aspettare la fine del run.
    try:
        import os, psycopg2  # type: ignore[import-untyped]
        from psycopg2.extras import Json as _Json  # type: ignore[import-untyped]
        _run_id = state.get("thread_id") or ""
        _dburl = os.environ.get("DATABASE_URL", "")
        if _run_id and _dburl:
            _step_base = int(state.get("iterations") or 0) * 1000
            _conn = psycopg2.connect(_dburl)
            try:
                with _conn.cursor() as _cur:
                    for _idx, (_block, _result) in enumerate(zip(pending, results)):
                        _t_name = _block.get("name", "")
                        _t_input = _block.get("input", {}) or {}
                        _t_result = _result.get("content", "")
                        _status = "failed" if _result.get("is_error") else "completed"
                        _cur.execute(
                            """INSERT INTO agent_steps
                               (id, run_id, step_index, tool_name, tool_input, tool_result, status, created_at)
                               VALUES (gen_random_uuid(), %s, %s, %s, %s, %s, %s, NOW())
                               ON CONFLICT DO NOTHING""",
                            (_run_id, _step_base + _idx, _t_name, _Json(_t_input), _t_result, _status),
                        )
                _conn.commit()
            finally:
                _conn.close()
    except Exception as _persist_exc:
        logger.warning("tool_dispatch_node: persistenza incrementale agent_steps fallita: %s", _persist_exc)

    new_chars = sum(len(r.get("content", "")) for r in results)
    if ctx_chars + new_chars > MAX_CONTEXT_CHARS:
        budget_per_tool = max(1500, (MAX_CONTEXT_CHARS - ctx_chars) // max(len(results), 1))
        results = [
            {
                **r,
                "content": _smart_truncate_lossless(
                    r["content"],
                    budget_per_tool,
                    source_kind="tool_result",
                    metadata={
                        "session_id": session_id,
                        "thread_id": str(state.get("thread_id") or ""),
                        "reason": "context_budget_cap",
                    },
                ),
            }
            for r in results
        ]
        logger.warning(
            "tool_dispatch_node: contesto vicino al limite (%d+%d chars), "
            "troncamento aggressivo a %d char/tool",
            ctx_chars, new_chars, budget_per_tool,
        )

    # ── PR-1: TODO reminder injection (anti-amnesia) ─────────────────────────
    # Se il plan e' attivo e siamo a una soglia di N tool use, appende
    # un blocco <system-reminder> con la checklist corrente al tool_result
    # HumanMessage. Counter resetta dopo injection.
    final_blocks = list(results)
    new_reminder_counter = int(state.get("since_last_todo_reminder", 0) or 0) + len(pending)
    if state.get("plan_phase_active"):
        from . import todo_reminder
        # Check soglia via cfg (cache TTL 60s)
        cfg_reminder = orchestrator_config.get()
        every_n = max(1, int(cfg_reminder.get("todo_reminder_every_n_steps", 5)))
        if new_reminder_counter >= every_n:
            reminder_run_id = state.get("thread_id") or ""
            reminder_text = todo_reminder.build_reminder_text(reminder_run_id) if reminder_run_id else None
            if reminder_text:
                todo_reminder.append_reminder_block(final_blocks, reminder_text)
                # Best-effort: traccia che i todos sono stati "visti" in questa iter
                try:
                    todo_store.increment_iteration_seen(reminder_run_id)
                except Exception:
                    pass
                new_reminder_counter = 0
                logger.debug(
                    "tool_dispatch_node: TODO reminder iniettato per run_id=%s",
                    reminder_run_id,
                )

    tool_msg = HumanMessage(
        content="", additional_kwargs={"anthropic_content": final_blocks},
    )

    # Live UX: emette un meta_step per OGNI tool eseguito in questa iter.
    # Senza, durante run lunghi (npm install, build, ecc.) la chat sembra
    # ferma fino a fine run. Il generator SSE in brain/grpc_server/main.py
    # converte questi "meta_steps" in eventi `meta_step` che la UI rende
    # come fumetti progressivi (kind=tool_executed, vedi agent-meta-step-card.tsx).
    _tool_steps: list[dict] = []
    for b, r in zip(pending, results):
        _ms_tool = b.get("name", "?")
        _ms_input = b.get("input", {}) or {}
        _ms_target = ""
        if isinstance(_ms_input, dict):
            for _k in ("path", "file_path", "abs_path", "command", "query", "pattern", "name", "tool_name"):
                _v = _ms_input.get(_k)
                if isinstance(_v, str) and _v:
                    _ms_target = _v if len(_v) <= 80 else (_v[:77] + "...")
                    break
        _ms_err = bool(r.get("is_error"))
        _ms_title = f"{'errore' if _ms_err else 'tool'} {_ms_tool}" + (f" — {_ms_target}" if _ms_target else "")
        _step = meta_steps.make(
            kind="tool_executed",
            title=_ms_title,
            payload={
                "tool": _ms_tool,
                "target": _ms_target,
                "is_error": _ms_err,
                "tool_use_id": b.get("id"),
            },
        )
        if _step:
            _tool_steps.append(_step)
            meta_steps.persist_async(state.get("thread_id"), _step)

    return {
        "messages": [tool_msg],
        "pending_tool_uses": [],
        "stop_reason": "tool_use",
        "since_last_todo_reminder": new_reminder_counter,
        "attachment_read_bytes": new_attachment_read_bytes,
        "meta_steps": _tool_steps,
    }


# ─── Routing condizionale post-executor ──────────────────────────────────────

def route_after_executor(state: AgentState) -> str:
    """Decide se iterare (tool_dispatch), verificare (verifier), o chiudere (learner).

    Safety cap: superato MAX_AGENT_ITERATIONS forza learner per evitare
    loop infiniti.
    Loop detection: stop_reason="loop_detected" forza chiusura immediata.

    PR-2: se plan_phase_active + verifier_enabled e end_turn, va al verifier
    (che poi decide se re-iterare o passare a reflection).
    """
    iterations = int(state.get("iterations") or 0)
    stop_reason = state.get("stop_reason")
    pending = state.get("pending_tool_uses") or []
    # Mig 0181: cap adattivo dal router_node, fallback a MAX_AGENT_ITERATIONS.
    iter_cap = int(state.get("iteration_budget") or 0) or MAX_AGENT_ITERATIONS
    if stop_reason == "loop_detected":
        logger.warning("route_after_executor: loop detected, chiusura forzata")
        return "learner"
    # G1 cap: l'executor stesso ha gia' segnalato di aver raggiunto il
    # numero massimo di re-execution G1 e ha emesso il messaggio assistant
    # esplicativo. Andiamo direttamente al learner senza altri giri.
    if stop_reason == "g1_cap_reached":
        logger.warning("route_after_executor: G1 cap raggiunto, chiusura forzata")
        return "learner"
    if iterations >= iter_cap:
        logger.warning(
            "route_after_executor: cap iterazioni adattivo raggiunto (iter=%d cap=%d complexity=%d)",
            iterations, iter_cap, int(state.get("complexity_score") or 0)
        )
        return "learner"
    if stop_reason == "tool_use" and pending:
        return "tool_dispatch"
    # PR-2: end_turn con plan_phase_active → verifier
    if state.get("plan_phase_active"):
        cfg = orchestrator_config.get()
        if cfg.get("verifier_enabled"):
            return "verifier"
    # G1: risposta puramente descrittiva su richiesta action-oriented.
    # Se il modello ha risposto senza tool call (end_turn, no pending) e
    # la richiesta originale era un'azione concreta, ri-manda all'executor
    # cosi' il nudge puo' attivarsi. Cap dal DB (agent.g1_max_nudges,
    # default 3): superato il cap NON si ri-manda piu' a executor; il cap
    # effettivo viene applicato dall'executor stesso che produce il
    # messaggio di stop esplicito (stop_reason="g1_cap_reached") gestito
    # dal ramo apposito sopra.
    #
    # Il contatore canonico per il routing G1 e' `g1_reroute_count`
    # (incrementato nell'executor a ogni re-entry G1) — usarlo qui invece
    # di `action_nudge_count` evita il loop infinito quando il nudge non
    # viene effettivamente iniettato (es. history contiene gia' tool call,
    # _has_tool_calls_in_history filtra) e action_nudge_count resta a 0.
    if not pending and stop_reason in ("end_turn", "stop", None):
        _reroute_count = int(state.get("g1_reroute_count") or 0)
        _max_nudges = _load_g1_max_nudges()
        if _reroute_count < _max_nudges:
            _msgs = state.get("messages") or []
            _first_human = next(
                (getattr(m, "content", "") for m in _msgs if hasattr(m, "type") and m.type == "human"),
                "",
            )
            if isinstance(_first_human, list):
                _first_human = " ".join(
                    b.get("text", "") for b in _first_human if isinstance(b, dict)
                )
            if _detect_action_request(str(_first_human)):
                _nudge_count_log = int(state.get("action_nudge_count") or 0)
                logger.warning(
                    "route_after_executor: G1 risposta descrittiva su action request "
                    "(iter=%d reroute=%d/%d nudge=%d) -> re-executor",
                    iterations, _reroute_count, _max_nudges, _nudge_count_log,
                )
                return "executor"
        else:
            logger.warning(
                "route_after_executor: G1 cap reroute raggiunto "
                "(iter=%d reroute=%d/%d) -> chiusura forzata via learner",
                iterations, _reroute_count, _max_nudges,
            )
    return "learner"


def route_after_verifier(state: AgentState) -> str:
    """Decide post-verifier: re-iterare (executor) o chiudere (learner/reflection).

    Logica:
    - Se stop_reason='end_turn' (verifier ha promosso il prossimo todo o
      finito tutti): vai a reflection
    - Se stop_reason='tool_use' (verifier ha iniettato retry o passato al
      prossimo todo): rientra in executor
    """
    iterations = int(state.get("iterations") or 0)
    iter_cap = int(state.get("iteration_budget") or 0) or MAX_AGENT_ITERATIONS
    if iterations >= iter_cap:
        logger.warning("route_after_verifier: cap iterazioni adattivo (iter=%d cap=%d), chiudo", iterations, iter_cap)
        return "learner"
    stop_reason = state.get("stop_reason")
    if stop_reason == "tool_use":
        return "executor"
    return "learner"


# ─── Nodo: reflection ────────────────────────────────────────────────────────

async def reflection_node(state: AgentState) -> dict[str, Any]:
    """Esegue la self-reflection post-esecuzione e calcola il punteggio qualita'.

    Attivo solo se:
    1. reflection_enabled=true nel DB (categoria 'reflection').
    2. Il template del prompt contiene il tag <reflection> (indica v2 XML).
    3. Il campionamento probabilistico (reflection_sample_rate dal DB) lo include.

    Tutti i parametri sono letti dal DB via reflection_config (cache TTL 60s).
    Nessuna variabile d'ambiente: le modifiche admin sono attive senza rideploy.

    Flusso:
    - Legge configurazione dal DB (enabled, sample_rate, timeout_s, model, ecc.).
    - Costruisce il prompt di valutazione (rubrica statica).
    - Chiama il provider LLM con max_tokens=400 e temperature=0.
    - Parsa la risposta JSON: {score, dimensions, weaknesses, suggestions}.
    - Calcola final_reward = (1-weight)*heuristic + weight*reflection_score.
    - Persiste in nexus_agent_reflections (fire-and-forget, non blocca).
    - Se score >= reflection_reasoning_bank_min_score, bridge verso reasoning_bank.
    """
    import datetime

    # ── Legge configurazione dal DB (cache TTL 60s, zero env vars) ──────────
    cfg = reflection_config.get()
    cfg_enabled: bool = bool(cfg["reflection_enabled"])
    cfg_sample_rate: float = float(cfg["reflection_sample_rate"])
    cfg_timeout_s: float = float(cfg["reflection_timeout_s"])
    cfg_model: str = str(cfg["reflection_model"])
    cfg_reward_weight: float = float(cfg["reflection_reward_weight"])

    # ── Guard: feature flag globale (DB) ────────────────────────────────────
    if not cfg_enabled:
        logger.debug("reflection_node: disabilitato (reflection_enabled=false in DB)")
        return {"reflection_score": None, "final_reward": None}

    # ── Guard: solo prompt XML v2 con tag <reflection> ──────────────────────
    system_text = state.get("system_text") or ""
    if "<reflection>" not in system_text:
        logger.debug("reflection_node: prompt senza tag <reflection>, skip")
        return {"reflection_score": None, "final_reward": None}

    # ── Guard: sampling probabilistico (dal DB) ──────────────────────────────
    if random.random() > cfg_sample_rate:
        logger.debug(
            "reflection_node: escluso per sampling (rate=%.2f da DB)", cfg_sample_rate
        )
        return {"reflection_score": None, "final_reward": None}

    # ── Raccolta dati dallo stato ────────────────────────────────────────────
    result = state.get("result") or ""
    stop_reason = state.get("stop_reason") or "end_turn"
    iterations = int(state.get("iterations") or 0)
    profile_name = state.get("profile_name")
    provider = state.get("provider_used")
    model = state.get("model_used")
    thread_id = state.get("thread_id") or str(uuid.uuid4())
    prompt_key = ""
    prompt_version = 1

    # Recupera prompt_key dalla registry tramite il profile
    if profile_name:
        prof = profile_loader.get_profile(profile_name)
        if prof is not None:
            prompt_key = prof.prompt_key
            # La versione viene persistita come 1 di default; la precisione esatta
            # ha importanza secondaria per la reflection (serve solo come metadato
            # di tracciamento nel PromptOptimizerWorker - Fase 3).

    # Testo del task originale (primo HumanMessage)
    messages = state.get("messages", [])
    task_input = ""
    for msg in messages:
        if isinstance(msg, HumanMessage):
            task_input = msg.content if hasattr(msg, "content") else str(msg)
            break

    if not result:
        logger.debug("reflection_node: result vuoto, skip valutazione")
        return {"reflection_score": None, "final_reward": None}

    # ── Chiamata LLM reflection ──────────────────────────────────────────────
    reflection_data: dict[str, Any] | None = None
    reflection_latency_ms: int = 0
    # Il modello usato e' quello configurato in DB (reflection_model)
    reflection_model_used = cfg_model

    if _providers is not None:
        sys_prompt, user_prompt = build_reflection_prompt(task_input, result)

        # Preferisce il provider Anthropic; fallback al provider attivo del run
        reflection_provider = "anthropic" if _providers._providers.get("anthropic") else (provider or "openai")  # type: ignore[attr-defined]

        t0 = time.monotonic()
        try:
            prov = _providers._providers.get(reflection_provider)  # type: ignore[attr-defined]
            if prov is not None and hasattr(prov, "generate_completion_async"):
                full_prompt = f"{sys_prompt}\n\n{user_prompt}"
                raw_result = await asyncio.wait_for(
                    prov.generate_completion_async(
                        cfg_model,
                        full_prompt,
                        max_tokens=400,
                        temperature=0.0,
                    ),
                    timeout=cfg_timeout_s,
                )
                raw_text = raw_result.content if hasattr(raw_result, "content") else str(raw_result)
                reflection_data = parse_reflection_response(raw_text)
                if reflection_data is None:
                    logger.warning(
                        "reflection_node: parsing fallito per thread=%s", thread_id
                    )
        except asyncio.TimeoutError:
            logger.warning(
                "reflection_node: timeout (%.1fs) per thread=%s", cfg_timeout_s, thread_id
            )
        except Exception as exc:
            logger.error("reflection_node: errore chiamata LLM: %s", exc)
        finally:
            reflection_latency_ms = int((time.monotonic() - t0) * 1000)
    else:
        logger.debug("reflection_node: _providers non configurato, skip chiamata LLM")

    # ── Calcolo reward finale ────────────────────────────────────────────────
    # Reward euristico (uguale logica del learner_node)
    if stop_reason == "error":
        heuristic = 0.0
    elif iterations >= (int(state.get("iteration_budget") or 0) or MAX_AGENT_ITERATIONS):
        heuristic = 0.3
    elif result:
        heuristic = 1.0
    else:
        heuristic = 0.4

    reflection_score: float | None = None
    final_reward: float | None = None

    if reflection_data is not None:
        reflection_score = reflection_data["score"]
        heuristic_weight = round(1.0 - cfg_reward_weight, 4)
        final_reward = round(
            heuristic_weight * heuristic
            + cfg_reward_weight * reflection_score,
            4,
        )
        logger.info(
            "reflection_node: thread=%s score=%.3f heuristic=%.2f final_reward=%.4f",
            thread_id, reflection_score, heuristic, final_reward,
        )
    else:
        # Nessuna reflection disponibile: il learner usa solo euristica
        final_reward = None

    # ── Persistenza asincrona in nexus_agent_reflections ────────────────────
    if reflection_data is not None and prompt_key:
        asyncio.ensure_future(
            _persist_reflection(
                run_id=thread_id,
                prompt_key=prompt_key,
                prompt_version=prompt_version,
                reflection_data=reflection_data,
                model_used=reflection_model_used,
                latency_ms=reflection_latency_ms,
            )
        )

    # ── Bridge verso reasoning_bank (score >= soglia da DB) ─────────────────
    reasoning_bank_min = float(cfg.get("reflection_reasoning_bank_min_score", 0.85))
    if (
        reflection_data is not None
        and reflection_score is not None
        and reflection_score >= reasoning_bank_min
        and reflection_data.get("suggestions")
    ):
        from .reasoning_bank import maybe_store_reflection_example
        asyncio.ensure_future(
            maybe_store_reflection_example(
                prompt_key=prompt_key,
                prompt_version=prompt_version,
                task_input=task_input,
                agent_output=result,
                reflection=reflection_data,
                profile_name=profile_name,
                lang=state.get("repo_lang"),
            )
        )

    return {
        "reflection_score": reflection_score,
        "reflection_dimensions": reflection_data.get("dimensions") if reflection_data else None,
        "reflection_weaknesses": reflection_data.get("weaknesses") if reflection_data else None,
        "reflection_suggestions": reflection_data.get("suggestions") if reflection_data else None,
        "final_reward": final_reward,
    }


async def _persist_reflection(
    run_id: str,
    prompt_key: str,
    prompt_version: int,
    reflection_data: dict[str, Any],
    model_used: str,
    latency_ms: int,
) -> None:
    """Persiste la reflection in nexus_agent_reflections. Fire-and-forget."""
    """Persiste la reflection in nexus_agent_reflections. Fire-and-forget."""
    try:
        from brain import db  # type: ignore[import]
        pool = db.get_pool()
        if pool is None:
            return
        import json
        async with pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO nexus_agent_reflections
                    (run_id, prompt_key, prompt_version, score, dimensions,
                     weaknesses, suggestions, model_used, latency_ms)
                VALUES ($1::uuid, $2, $3, $4, $5::jsonb, $6, $7, $8, $9)
                """,
                run_id,
                prompt_key,
                prompt_version,
                reflection_data["score"],
                json.dumps(reflection_data.get("dimensions") or {}),
                reflection_data.get("weaknesses") or [],
                reflection_data.get("suggestions") or [],
                model_used,
                latency_ms,
            )
    except Exception as exc:
        logger.error("_persist_reflection: errore DB per run_id=%s: %s", run_id, exc)


# ─── Nodo: learner ───────────────────────────────────────────────────────────

async def learner_node(state: AgentState) -> dict[str, Any]:
    """Salva l'interazione in SQLite/Qdrant e chiude il loop Q-learning.

    Oltre a persistere l'embedding per retrieval, invia un `submit_feedback`
    al `AgentRouterClient` con un reward euristico derivato dallo stato finale
    (successo = `stop_reason == "end_turn"` senza errori). Il Q-value e' cosi'
    aggiornato in nexus-orchestrator per le decisioni future.

    Cattura anche le metriche estese (token, costo, latency) nel return.
    """
    import datetime

    messages = state.get("messages", [])
    thread_id = state.get("thread_id", str(uuid.uuid4()))
    task_type = state.get("user_intent", "chat")
    behavior_mode = state.get("behavior_mode", "bilanciata")
    result = state.get("result", "")
    provider = state.get("provider_used")
    model = state.get("model_used")
    latency_ms = state.get("latency_ms")
    token_usage = state.get("token_usage")
    created_at = state.get("created_at")
    completed_at = datetime.datetime.now(datetime.timezone.utc).isoformat()

    # Recupera testo input utente
    user_input = ""
    for msg in messages:
        if isinstance(msg, HumanMessage):
            user_input = msg.content if hasattr(msg, "content") else str(msg)
            break

    qdrant_id: str | None = None

    # Stima reward preliminare (euristica base, senza reflection — disponibile subito).
    # Usata per filtrare il salvataggio Qdrant: interazioni di bassa qualita'
    # non devono inquinare il RAG (esempi falliti degradano il retrieval futuro).
    stop_reason_pre = state.get("stop_reason") or "end_turn"
    prelim_reward = (
        1.0 if stop_reason_pre == "end_turn" and result
        else 0.4 if stop_reason_pre == "end_turn"
        else 0.0 if stop_reason_pre == "error"
        else 0.3  # cap iterazioni o altro
    )

    # Leggi config learning dal DB (cache 60s). Stessa convenzione di reflection_config.
    lcfg = _get_learning_config()
    save_to_qdrant = lcfg["auto_extract"] and prelim_reward >= lcfg["min_confidence"]

    if not lcfg["auto_extract"]:
        logger.debug("learner_node: salvataggio Qdrant saltato (learning_auto_extract=false)")
    elif prelim_reward < lcfg["min_confidence"]:
        logger.debug(
            "learner_node: salvataggio Qdrant saltato (reward_prelim=%.2f < min_confidence=%.2f, stop=%s)",
            prelim_reward, lcfg["min_confidence"], stop_reason_pre,
        )

    if not result:
        logger.warning(
            "learner_node: thread=%s result vuoto (stop=%s), skip Qdrant/SQLite",
            thread_id, stop_reason_pre,
        )

    # Salva embedding in Qdrant solo se qualita' sufficiente e auto_extract abilitato
    if _retriever is not None and user_input and result and save_to_qdrant:
        interaction_text = f"Input: {user_input}\nOutput: {result}"
        qdrant_id = str(uuid.uuid4())
        payload: dict[str, Any] = {
            "thread_id": thread_id,
            "task_type": task_type,
            "behavior_mode": behavior_mode,
            "provider": provider,
            "model": model,
            "input_preview": user_input[:200],
            "output_preview": result[:200] if result else "",
        }
        stored = _retriever.store_interaction_vector(qdrant_id, interaction_text, payload)
        if not stored:
            qdrant_id = None

    # Salva in PostgreSQL (brain_learning_interactions)
    if _storage is not None and user_input:
        try:
            _storage.save_interaction(
                thread_id=thread_id,
                task_type=task_type,
                behavior_mode=behavior_mode,
                user_input=user_input,
                agent_output=result or "",
                provider=provider,
                model=model,
                latency_ms=latency_ms,
                token_usage=token_usage,
                qdrant_id=qdrant_id,
                metadata={"iterations": state.get("iterations", 1)},
            )
        except Exception as exc:
            logger.warning("learner_node: salvataggio PostgreSQL fallito thread=%s: %s", thread_id, exc)

    # ── Feedback al router Q-Learning ──────────────────────────────────────
    # Reward: usa final_reward dal reflection_node se disponibile,
    # altrimenti calcola l'euristica standard.
    #   Euristica:
    #   - end_turn pulito              -> 1.0
    #   - end_turn ma stringa vuota    -> 0.4
    #   - error                        -> 0.0
    #   - cap iterazioni raggiunto     -> 0.3
    #
    #   Reward fuso (se reflection disponibile):
    #   final_reward = 0.7 * heuristic + 0.3 * reflection_score
    profile_name = state.get("profile_name")
    stop_reason = state.get("stop_reason") or "end_turn"
    iterations = int(state.get("iterations") or 0)
    if _agent_router is not None and profile_name:
        # Calcola reward euristico (usato come fallback e per il log)
        if stop_reason == "error":
            heuristic_reward = 0.0
        elif iterations >= MAX_AGENT_ITERATIONS:
            heuristic_reward = 0.3
        elif result:
            heuristic_reward = 1.0
        else:
            heuristic_reward = 0.4

        # Usa il reward fuso dal reflection_node se disponibile
        final_reward = state.get("final_reward")
        reward = final_reward if final_reward is not None else heuristic_reward

        reflection_score = state.get("reflection_score")
        if reflection_score is not None:
            logger.info(
                "learner_node: reward fuso profile=%s heuristic=%.2f "
                "reflection=%.3f final=%.4f",
                profile_name, heuristic_reward, reflection_score, reward,
            )
        try:
            new_q = await _agent_router.submit_feedback(
                task_id=thread_id,
                task_type=task_type,
                agent_type=profile_name,
                reward=reward,
                duration_ms=int(latency_ms or 0),
                is_terminal=True,
            )
            logger.info(
                "learner_node: Q-feedback profile=%s reward=%.4f new_q=%.3f",
                profile_name, reward, new_q,
            )
        except Exception as exc:
            logger.warning("learner_node: feedback Q-learning fallito thread=%s: %s", thread_id, exc)

    logger.info(
        "learner_node: thread=%s task=%s latency=%.0fms qdrant_id=%s",
        thread_id, task_type, latency_ms or 0, qdrant_id,
    )

    return {
        "completed_at": completed_at,
    }


# ─── PR-3 Codex + Cursor: project_instructions + available_subagents blocks ─

def _build_pr3_system_blocks(state: AgentState) -> str:
    """Compone i blocchi extra da appendere al system_text:
      - <project_instructions>: contenuto di .nexus/project-instructions.md
      - <available_subagents>: catalogo kind disponibili per auto-delegation
    Best-effort: ritorna stringa vuota se nulla da iniettare.
    """
    blocks: list[str] = []
    project_id = state.get("project_id") or ""
    # 1. Project instructions (AGENTS.md / CLAUDE.md style).
    try:
        from . import orchestrator_config, project_instructions_loader
        import os as _os
        cfg = orchestrator_config.get()
        if cfg.get("plan_phase_enabled", False) and project_id:
            url = _os.environ.get("DATABASE_URL", "")
            if url:
                import psycopg2  # type: ignore[import-untyped]
                with psycopg2.connect(url) as conn:
                    proj_root = None
                    try:
                        with conn.cursor() as cur:
                            cur.execute(
                                "SELECT absolute_path FROM workspaces WHERE project_id=%s AND is_primary=true LIMIT 1",
                                (project_id,),
                            )
                            row = cur.fetchone()
                            proj_root = row[0] if row else None
                    except Exception:
                        proj_root = None
                    instr = project_instructions_loader.load_project_instructions(
                        conn, project_id, proj_root,
                    )
                    if instr:
                        blocks.append(instr)
    except Exception:
        pass
    # 2. Available subagents (Cursor pattern auto-delegation).
    try:
        from . import orchestrator_config, subagent_store, prompt_registry, prompt_renderer
        cfg = orchestrator_config.get()
        if cfg.get("subagents_enabled", False) and cfg.get("auto_delegation_enabled", True):
            proj_root = None
            url = __import__("os").environ.get("DATABASE_URL", "")
            if url and project_id:
                try:
                    import psycopg2  # type: ignore[import-untyped]
                    with psycopg2.connect(url) as conn:
                        with conn.cursor() as cur:
                            cur.execute(
                                "SELECT absolute_path FROM workspaces WHERE project_id=%s AND is_primary=true LIMIT 1",
                                (project_id,),
                            )
                            row = cur.fetchone()
                            proj_root = row[0] if row else None
                except Exception:
                    pass
            kinds = subagent_store.list_enabled_kinds(proj_root)
            if kinds:
                wl = (cfg.get("subagent_kinds_whitelist") or "").split(",")
                wl = [w.strip() for w in wl if w.strip()]
                if wl:
                    kinds = [k for k in kinds if k.get("kind") in wl]
            if kinds:
                lines = []
                for k in kinds:
                    bg = " (background)" if k.get("is_background") else ""
                    lines.append(f"- {k['kind']}{bg}: {k.get('description','')}")
                subagents_block = "\n".join(lines)
                tpl = prompt_registry.get_prompt("system.available_subagents_block") or ""
                if tpl:
                    rendered = prompt_renderer.render(tpl, {
                        "subagents_block": subagents_block,
                        "max_parallel": cfg.get("max_parallel_subagents", 3),
                    })
                    blocks.append(rendered)
                else:
                    blocks.append(f"<available_subagents>\n{subagents_block}\n</available_subagents>")
    except Exception:
        pass
    return "\n\n".join(b for b in blocks if b)


# ─── Funzione di routing condizionale ────────────────────────────────────────

def route_by_task_type(state: AgentState) -> str:
    """Routing condizionale: mappa task_type al nodo executor."""
    # Tutti i task_type validi vanno verso executor
    return "executor"
