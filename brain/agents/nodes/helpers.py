"""Helper PURI dei nodi LangGraph di Nexus.

Questo modulo raccoglie le funzioni di supporto SENZA stato mutabile di servizio
(niente _embeddings/_providers/...): rilevamento intent, budget iterazioni,
gestione lingua, thinking, prezzi, compressione/contesto deterministica e
conversione messaggi. Estratto da nodes.py (refactoring god-file) mantenendo
comportamento e nomi identici. Le funzioni che dipendono dai servizi globali
restano nel package __init__ (namespace brain.agents.nodes).
"""
from __future__ import annotations

import asyncio
import hashlib
import json
import logging
import os
import random
import re
import time
import uuid
from typing import Any

from langchain_core.messages import AIMessage, HumanMessage, ToolMessage
from brain.utils.db_pool import get_db_url

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
        db_url = get_db_url()  # regola G: niente fallback hardcoded
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


# ── Floor selettivo del tier per i task agentici (cache 60s) ─────────────────
# Un task che entra nel loop tool-use ed e' "pesante" (agentic_score alto o
# budget iterazioni alto) merita un modello tool-robust anche quando l'utente ha
# scelto behavior_mode "veloce"/"economica": sotto un modello lite il loop
# agentico (tool forcing multi-step) tende a fallire. Il floor scatta su segnali
# SEMANTICI (agentic_score dal classifier LLM) o sul budget gia' calcolato, NON
# su keyword (a differenza del vecchio is_risky_task, rimosso). Settings DB (reg. G).
_TIER_FLOOR_CACHE: dict[str, Any] = {"loaded_at": 0.0, "config": None}
_TIER_FLOOR_TTL_SEC = 60.0
_TIER_FLOOR_DEFAULTS = {
    "enabled": True,
    "agentic_score_min": 0.6,
    "iteration_budget_min": 160,
    "mode": "bilanciata",
}
# Modalita' "economiche" su cui si applica il floor; le altre restano invariate.
_TIER_FLOOR_LOW_MODES = {"veloce", "economica"}


def _load_tier_floor_config() -> dict[str, Any]:
    """Carica agent.tier_floor.* dal DB con cache 60s. Mai solleva (degraded mode)."""
    now = time.time()
    if _TIER_FLOOR_CACHE["config"] is not None and (now - _TIER_FLOOR_CACHE["loaded_at"]) < _TIER_FLOOR_TTL_SEC:
        return _TIER_FLOOR_CACHE["config"]
    config = dict(_TIER_FLOOR_DEFAULTS)
    try:
        import psycopg2
        db_url = get_db_url()  # regola G: niente fallback hardcoded sull'URL
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                cur.execute("SELECT key, value FROM settings WHERE key LIKE 'agent.tier_floor.%%'")
                for key, value in cur.fetchall():
                    if key == "agent.tier_floor.enabled":
                        config["enabled"] = str(value).strip().lower() not in ("false", "0", "off", "no")
                    elif key == "agent.tier_floor.agentic_score_min":
                        try:
                            config["agentic_score_min"] = float(value)
                        except (TypeError, ValueError):
                            pass
                    elif key == "agent.tier_floor.iteration_budget_min":
                        try:
                            config["iteration_budget_min"] = int(value)
                        except (TypeError, ValueError):
                            pass
                    elif key == "agent.tier_floor.mode":
                        if str(value).strip():
                            config["mode"] = str(value).strip()
    except Exception as exc:
        logger.warning("tier_floor: load DB fallito, uso defaults (%s)", exc)
    _TIER_FLOOR_CACHE["config"] = config
    _TIER_FLOOR_CACHE["loaded_at"] = now
    return config


def apply_agentic_tier_floor(behavior_mode: str, state: dict, cfg: dict | None = None) -> str:
    """Ritorna il behavior_mode effettivo applicando il floor selettivo.

    Se il task e' agentico "pesante" (agentic_score >= soglia OPPURE
    iteration_budget >= soglia) e la modalita' richiesta e' "veloce"/"economica",
    eleva al `mode` del floor (default "bilanciata"), cosi' il routing sceglie un
    modello tool-robust invece di un lite. Altrimenti lascia behavior_mode
    invariato (small-talk e task semplici rispettano la modalita' scelta).
    Mai solleva.
    """
    bm = (behavior_mode or "").strip().lower()
    if bm not in _TIER_FLOOR_LOW_MODES:
        return behavior_mode
    cfg = cfg or _load_tier_floor_config()
    if not cfg.get("enabled", True):
        return behavior_mode
    try:
        score = float(state.get("agentic_score") or 0.0)
    except (TypeError, ValueError):
        score = 0.0
    try:
        budget = int(state.get("iteration_budget") or 0)
    except (TypeError, ValueError):
        budget = 0
    if score >= float(cfg.get("agentic_score_min", 0.6)) or budget >= int(cfg.get("iteration_budget_min", 160)):
        floor_mode = str(cfg.get("mode") or "bilanciata")
        logger.info(
            "tier_floor: task agentico (agentic_score=%.2f budget=%d) mode=%s -> %s",
            score, budget, bm, floor_mode,
        )
        return floor_mode
    return behavior_mode


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


# ── ADR 0018 (b): tool_choice forcing decisionale (cache 60s) ───────────────
# Forzare `tool_choice` nei turni d'azione previene alla radice lo stop
# narrativo (il modello chiude con solo testo invece di emettere tool call).
# Due settings DB (regola G): abilitazione globale + soglia iterazione oltre la
# quale NON si forza piu' (lasciando al modello la liberta' di chiudere il
# task). I default valgono SOLO se il DB e' down (get_*_setting non solleva).
_TC_FORCING_CACHE: dict[str, Any] = {
    "loaded_at": 0.0,
    "enabled": None,
    "max_iteration": None,
}
_TC_FORCING_TTL_SEC = 60.0
_TC_FORCING_DEFAULT_ENABLED = True
_TC_FORCING_DEFAULT_MAX_ITER = 2

# Stili di tool_choice (cap.tool_choice_style) che supportano il forcing. Lo
# style "none" e "openai_auto" non permettono di OBBLIGARE una tool call:
# per quei modelli il forcing va trattato come non applicabile.
_TC_FORCING_SUPPORTED_STYLES: frozenset[str] = frozenset({
    "anthropic_any",
    "openai_required",
    "google_function_calling_any",
})


def _load_tool_choice_forcing_config() -> tuple[bool, int]:
    """Legge agent.tool_choice_forcing_enabled / _max_iteration dal DB (cache 60s).

    Ritorna `(enabled, max_iteration)`. Default safe se DB down:
    `(_TC_FORCING_DEFAULT_ENABLED, _TC_FORCING_DEFAULT_MAX_ITER)`. Mai solleva.
    """
    now = time.time()
    if (
        _TC_FORCING_CACHE["enabled"] is not None
        and (now - _TC_FORCING_CACHE["loaded_at"]) < _TC_FORCING_TTL_SEC
    ):
        return bool(_TC_FORCING_CACHE["enabled"]), int(_TC_FORCING_CACHE["max_iteration"])
    enabled = _TC_FORCING_DEFAULT_ENABLED
    max_iter = _TC_FORCING_DEFAULT_MAX_ITER
    try:
        from brain.utils.settings_db import get_bool_setting, get_int_setting
        enabled = bool(
            get_bool_setting("agent.tool_choice_forcing_enabled", _TC_FORCING_DEFAULT_ENABLED)
        )
        max_iter = int(
            get_int_setting("agent.tool_choice_forcing_max_iteration", _TC_FORCING_DEFAULT_MAX_ITER)
        )
        if max_iter < 0:
            max_iter = _TC_FORCING_DEFAULT_MAX_ITER
    except Exception as exc:
        logger.warning(
            "tool_choice_forcing: load DB fallito, uso default (%s, %d) (%s)",
            _TC_FORCING_DEFAULT_ENABLED, _TC_FORCING_DEFAULT_MAX_ITER, exc,
        )
    _TC_FORCING_CACHE["enabled"] = enabled
    _TC_FORCING_CACHE["max_iteration"] = max_iter
    _TC_FORCING_CACHE["loaded_at"] = now
    return enabled, max_iter


def should_force_tool_choice(
    *,
    tools_available: bool,
    action_oriented: bool,
    iteration: int,
    in_discovery_phase: bool,
    provider_supports_forcing: bool,
    enabled: bool = True,
    max_iteration: int = _TC_FORCING_DEFAULT_MAX_ITER,
) -> bool:
    """Funzione PURA: decide se forzare `tool_choice` per il turno corrente.

    ADR 0018 leva 2. Ritorna True solo quando TUTTE le condizioni sono vere:
      - il flag DB `enabled` e' attivo;
      - ci sono tool disponibili nel turno (`tools_available`);
      - il task e' action-oriented (richiesta d'azione dell'utente o intento);
      - siamo entro la soglia di iterazione (`iteration <= max_iteration`),
        cosi' dopo i primi turni il modello resta libero di chiudere il task;
      - NON siamo in una fase di discovery gia' gestita separatamente
        (M16 espone solo nexus_mcp_tool_search e forza gia' la search);
      - il provider/modello supporta il forcing (`provider_supports_forcing`).

    Nessuna lettura DB qui dentro: i parametri `enabled`/`max_iteration` vanno
    passati dal chiamante (caricati via `_load_tool_choice_forcing_config`).
    Cosi' la funzione resta deterministica e testabile in isolamento.
    """
    if not enabled:
        return False
    if not tools_available:
        return False
    if not action_oriented:
        return False
    if in_discovery_phase:
        return False
    if not provider_supports_forcing:
        return False
    if iteration > max_iteration:
        return False
    return True


def turn_action_oriented(state) -> bool:
    """Punto unico (regola L): il TURNO CORRENTE richiede azione con tool?

    Fonte autoritativa: il campo `action_oriented` calcolato da router_node
    dalla semantica del classifier LLM del turno corrente (requires_tools /
    agentic_score, soglia DB `routing.action_oriented_min_agentic_score`).

    Sostituisce le euristiche testuali locali (`_detect_action_request` sul
    PRIMO messaggio della history) sparse in 5 call site: in una sessione
    iniziata con una richiesta d'azione OGNI turno successivo risultava
    "azione" per sempre. Incidente osservato 2026-06-10: "riassumi in due
    righe cosa hai sistemato" valutato sul primo messaggio (un crash da
    debuggare) -> tool_choice forzato -> ri-esecuzione di npm run dev invece
    della risposta testuale.

    Default conservativo True quando il campo manca (run ripresi da checkpoint
    creati prima dell'introduzione del campo): mantiene attivi i guard
    anti-descrittivi; il transitorio si esaurisce coi run nuovi.
    """
    v = state.get("action_oriented")
    if v is None:
        return True
    return bool(v)


def provider_style_supports_forcing(tool_choice_style: str | None) -> bool:
    """True se lo style di tool_choice del provider permette di OBBLIGARE una
    tool call (anthropic_any / openai_required / google_function_calling_any).

    Pura, niente DB: lo style arriva dalla ProviderCapability gia' caricata.
    """
    if not tool_choice_style:
        return False
    return tool_choice_style in _TC_FORCING_SUPPORTED_STYLES


def structural_unfulfilled_signal(
    *,
    had_tools_available: bool,
    no_tool_call_this_turn: bool,
    action_oriented: bool,
    iteration: int,
    max_iteration: int,
) -> bool:
    """Funzione PURA: segnale STRUTTURALE di stop prematuro (ADR 0018 leva 1/c).

    Identifica il caso "BookingPage" (modello che annuncia un'azione e chiude
    il turno senza emettere alcuna tool call) SENZA guardare i verbi del testo:

      had_tools_available AND no_tool_call_this_turn AND action_oriented
      AND iteration <= max_iteration

    `had_tools_available`: nel turno il modello aveva tool a disposizione
        (tools_json non vuoto).
    `no_tool_call_this_turn`: il turno si e' chiuso senza tool_use pendenti
        (stop_reason end_turn/stop/None e nessun pending_tool_uses).
    `action_oriented`: la richiesta utente e' un'azione concreta (segnale di
        contesto del task, non un'euristica sull'output del modello).

    Deterministico, indipendente da lingua e modello. Il cap di reroute
    (g1_reroute_count) resta a carico del chiamante per evitare loop.
    """
    if not had_tools_available:
        return False
    if not no_tool_call_this_turn:
        return False
    if not action_oriented:
        return False
    if iteration > max_iteration:
        return False
    return True


# ── Tool task_complete: chiusura DICHIARATA dal modello (WAVE 3) ─────────────
# Tool brain-only (NON eseguito dal ToolRunner): il modello lo chiama per
# DICHIARARE l'esito del turno in modo strutturato e indipendente dalla lingua.
# Sostituisce l'inferenza lessicale dell'esito (_detect_unfulfilled_intent ~150
# frasi it/en, resigned_patterns) con un segnale esplicito. La dichiarazione e'
# un segnale PREFERITO ma gated dai fatti: outcome=done senza azioni produttive
# non e' fidato ciecamente (vedi route_after_executor).
TASK_COMPLETE_TOOL_NAME = "task_complete"

TASK_COMPLETE_TOOL: dict = {
    "name": TASK_COMPLETE_TOOL_NAME,
    "description": (
        "Chiudi il turno dichiarando l'esito in modo strutturato. Chiamalo "
        "quando hai finito (o sei bloccato), invece di rispondere solo a parole. "
        "outcome='done' se il compito e' completato; 'blocked' se non puoi "
        "proseguire per una causa esterna (dipendenza/credenziale/permesso/"
        "servizio mancante); 'needs_input' se ti serve una decisione dell'utente. "
        "summary: 1-2 frasi sull'esito. next_step: il prossimo passo concreto se "
        "non done. blocked_by: cosa ti blocca se blocked."
    ),
    "input_schema": {
        "type": "object",
        "properties": {
            "outcome": {
                "type": "string",
                "enum": ["done", "blocked", "needs_input"],
            },
            "summary": {"type": "string"},
            "next_step": {"type": "string"},
            "blocked_by": {"type": "string"},
        },
        "required": ["outcome", "summary"],
    },
}

_VALID_OUTCOMES = frozenset({"done", "blocked", "needs_input"})


def normalize_declared_outcome(tool_input: dict | None) -> dict | None:
    """Valida/normalizza l'input di task_complete (punto unico). None se invalido
    (outcome fuori enum o input non-dict): il chiamante ricade sui segnali
    strutturali/lessicali come se la dichiarazione non ci fosse."""
    if not isinstance(tool_input, dict):
        return None
    outcome = str(tool_input.get("outcome", "")).strip().lower()
    if outcome not in _VALID_OUTCOMES:
        return None
    out: dict = {"outcome": outcome, "summary": str(tool_input.get("summary", "")).strip()}
    for k in ("next_step", "blocked_by"):
        v = tool_input.get(k)
        if v:
            out[k] = str(v).strip()
    return out


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
    # Esplorazione filesystem di SOLA lettura: senza queste voci un loop di
    # list_files/grep/read_file_lines/search_in_files alternati azzerava il
    # contatore anti-loop a ogni iterazione (viste come "produttive"),
    # permettendo decine di letture consecutive senza mai scrivere -> context
    # explosion (incidente osservato: 58 step di sola esplorazione, ctx al 205%).
    # Sono TUTTI i tool read-only del filesystem (read_file, read_file_lines,
    # list_files, grep, search_in_files) — vanno tenuti allineati a quelli reali.
    "list_files", "grep", "read_file_lines", "search_in_files",
    # Discovery meta NON produttiva: cerca altri tool MCP ma non scrive ne'
    # risponde. Senza questa voce un loop di sole ricerche-tool azzererebbe
    # ad ogni iterazione il contatore anti-loop (visto come "produttivo"),
    # rendendo il loop infinito (es. gemini-flash-lite che cerca ossessivamente
    # tool per una domanda conversazionale invece di rispondere).
    "nexus_mcp_tool_search",
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


# ── Cache flag progress_controller (TTL 60s) ────────────────────────────────
# Interruttore del punto unico di controllo avanzamento (progress_controller):
# quando attivo, gli stalli del ciclo agentico seguono la gerarchia coordinata
# guida(forza-azione) -> escalate -> abort-verso-verifica invece di abortire
# subito. Default true (regola G: niente hardcode, DB unica fonte). OFF = ripristina
# il comportamento legacy (abort immediato) senza redeploy.
_PROGRESS_CTRL_CACHE: dict[str, Any] = {"loaded_at": 0.0, "enabled": None}
_PROGRESS_CTRL_TTL_SEC = 60.0
_PROGRESS_CTRL_DEFAULT = True


def _load_progress_controller_enabled() -> bool:
    """Legge agent.progress_controller_enabled dal DB con cache 60s.

    Ritorna il default (True) se il DB e' irraggiungibile o la chiave non esiste:
    get_bool_setting non solleva mai.
    """
    now = time.time()
    cached = _PROGRESS_CTRL_CACHE["enabled"]
    if cached is not None and (now - _PROGRESS_CTRL_CACHE["loaded_at"]) < _PROGRESS_CTRL_TTL_SEC:
        return bool(cached)
    try:
        from brain.utils.settings_db import get_bool_setting
        value = bool(get_bool_setting("agent.progress_controller_enabled", _PROGRESS_CTRL_DEFAULT))
    except Exception as exc:
        logger.warning(
            "progress_controller_enabled: load DB fallito, uso default %s (%s)",
            _PROGRESS_CTRL_DEFAULT, exc,
        )
        value = _PROGRESS_CTRL_DEFAULT
    _PROGRESS_CTRL_CACHE["enabled"] = value
    _PROGRESS_CTRL_CACHE["loaded_at"] = now
    return value


def _load_repeated_action_force_diagnose_enabled() -> bool:
    """Legge agent.repeated_action_force_diagnose_enabled dal DB (default True).

    Abilita lo stadio intermedio force_diagnose del progress_controller per
    l'azione ripetuta (mig 0386). get_bool_setting non solleva mai: default
    sicuro se il DB e' irraggiungibile.
    """
    try:
        from brain.utils.settings_db import get_bool_setting
        return bool(
            get_bool_setting("agent.repeated_action_force_diagnose_enabled", True)
        )
    except Exception as exc:
        logger.warning(
            "repeated_action_force_diagnose_enabled: load DB fallito, default True (%s)",
            exc,
        )
        return True


# ── Cache reminder lingua resiliente (TTL 60s) ──────────────────────────────
# Bug #88: a contesto saturo (>400K token) i modelli small con forte recency
# bias ignorano la direttiva di lingua presente solo in testa al system prompt
# e rispondono in una lingua diversa da quella dell'utente (es. inglese su
# richiesta italiana, o cinese allucinando l'identita'). Iniettiamo SEMPRE un
# reminder di lingua in coda al system_text e in coda all'ultimo HumanMessage
# (recency), coprendo cosi' anche i profili custom e i template senza direttiva.
# Il reminder NON impone una lingua fissa: impone di seguire la lingua del
# messaggio utente (regola: la lingua di risposta e' quella della richiesta).
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
    "Rispondi SEMPRE nella STESSA lingua del messaggio dell'utente "
    "(la lingua dell'ultima richiesta in chat). Se l'utente scrive in "
    "italiano rispondi in italiano, se scrive in inglese rispondi in "
    "inglese, e cosi' via. NON cambiare lingua per via del contesto, del "
    "codice, della documentazione o degli allegati: la lingua di risposta "
    "e' SOLO quella dell'utente."
)

# ── Forced RAG reminder (ADR 0016 Fase A.4) ─────────────────────────────────
# Quando `est_tokens > forced_rag_threshold_ratio * model.context_window`, iniettiamo
# nel system prompt + ultimo HumanMessage un'istruzione assertiva che obbliga
# l'agente a usare `nexus_search_semantic` prima di rispondere, invece di
# assumere di vedere tutto il contesto. Stesso pattern di `_inject_language_reminder`
# per riusare l'iniezione idempotente (marker univoco, no doppi insert).
_RAG_REMINDER_MARKER = "[[NEXUS_FORCED_RAG_REMINDER]]"
_RAG_REMINDER_CACHE: dict[str, Any] = {
    "loaded_at": 0.0,
    "threshold_ratio": None,
    "text": None,
}
_RAG_REMINDER_TTL_SEC = 60.0
_RAG_REMINDER_DEFAULT_RATIO = 0.40
_RAG_REMINDER_DEFAULT_TEXT = (
    "Il contesto disponibile e' parzialmente offloadato in tool_results_chunks. "
    "Prima di rispondere a richieste che richiedono dettagli specifici, "
    "chiama nexus_search_semantic(query=...). Non assumere di vedere tutto "
    "il contesto: chiedi quello che ti serve."
)


def _load_forced_rag_reminder() -> tuple[float, str]:
    """Legge agent.context.forced_rag_threshold_ratio / _reminder_text dal DB (cache 60s).

    Ritorna `(threshold_ratio, reminder_text)`. Default safe se DB down:
    `(0.40, _RAG_REMINDER_DEFAULT_TEXT)`. Mai solleva.
    """
    now = time.time()
    cached_ratio = _RAG_REMINDER_CACHE["threshold_ratio"]
    if cached_ratio is not None and (
        now - _RAG_REMINDER_CACHE["loaded_at"]
    ) < _RAG_REMINDER_TTL_SEC:
        return float(cached_ratio), str(_RAG_REMINDER_CACHE["text"])
    ratio = _RAG_REMINDER_DEFAULT_RATIO
    text = _RAG_REMINDER_DEFAULT_TEXT
    try:
        from brain.utils.settings_db import get_setting
        raw_ratio = get_setting(
            "agent.context.forced_rag_threshold_ratio",
            str(_RAG_REMINDER_DEFAULT_RATIO),
        )
        ratio = max(0.0, min(0.99, float(raw_ratio)))
        text = (
            get_setting(
                "agent.context.forced_rag_reminder_text",
                _RAG_REMINDER_DEFAULT_TEXT,
            ).strip()
            or _RAG_REMINDER_DEFAULT_TEXT
        )
    except Exception as exc:
        logger.warning(
            "forced_rag_reminder: load DB fallito, uso default (%s)", exc
        )
    _RAG_REMINDER_CACHE["threshold_ratio"] = ratio
    _RAG_REMINDER_CACHE["text"] = text
    _RAG_REMINDER_CACHE["loaded_at"] = now
    return ratio, text


def _inject_forced_rag_reminder(
    messages: list[Any],
    system_text: str,
    est_tokens: int,
    window: int,
) -> tuple[list[Any], str]:
    """Inietta il forced RAG reminder se `est_tokens > ratio * window`.

    Pattern speculare a `_inject_language_reminder` (marker idempotente,
    iniezione in TESTA al system + recency su ultimo HumanMessage). Quando il
    context e' grande, l'agente viene istruito a recuperare on-demand invece
    di assumere visione completa.

    Idempotente: ricaricare la stessa funzione non duplica il reminder.
    Ritorna `(messages, system_text)` invariati se sotto soglia o se ratio<=0.
    """
    if window <= 0 or est_tokens <= 0:
        return messages, system_text
    ratio, reminder_text = _load_forced_rag_reminder()
    if ratio <= 0 or not reminder_text:
        return messages, system_text
    threshold = int(window * ratio)
    if est_tokens < threshold:
        return messages, system_text

    base_system = system_text or ""
    if _RAG_REMINDER_MARKER not in base_system:
        rag_block = f"### RECUPERO ON-DEMAND DEL CONTESTO ###\n{reminder_text}"
        new_system = (
            f"{_RAG_REMINDER_MARKER}\n{rag_block}\n\n{base_system}\n\n{rag_block}"
        )
    else:
        new_system = system_text

    new_messages = messages
    for idx in range(len(messages) - 1, -1, -1):
        msg = messages[idx]
        if not isinstance(msg, HumanMessage):
            continue
        content = getattr(msg, "content", None)
        if not isinstance(content, str):
            break
        if _RAG_REMINDER_MARKER in content:
            break
        # Crea copia (no mutate sullo stato condiviso).
        appended = f"{content}\n\n{_RAG_REMINDER_MARKER} {reminder_text}"
        new_msg = HumanMessage(content=appended)
        new_messages = list(messages)
        new_messages[idx] = new_msg
        break

    logger.info(
        "forced_rag_reminder: iniettato (est_tokens=%d window=%d threshold=%d ratio=%.2f)",
        est_tokens, window, threshold, ratio,
    )
    return new_messages, new_system


def _load_language_reminder() -> tuple[bool, str]:
    """Legge agent.language_reminder_enabled / _text dal DB con cache 60s.

    Ritorna (enabled, text). I default sicuri (True, "rispondi nella lingua
    dell'utente") valgono SOLO se il DB e' irraggiungibile o la chiave non
    esiste: get_bool_setting /
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
    # Il vincolo di lingua va messo in TESTA, non in coda: i modelli con forte
    # bias linguistico (es. deepseek-chat -> cinese) ignorano una direttiva in
    # fondo a un system prompt lungo / con context compresso. Come PRIMA
    # istruzione (e ribadita in coda) diventa molto piu' difficile da ignorare.
    base_system = system_text or ""
    if _LANG_REMINDER_MARKER not in base_system:
        _lang_block = f"### LINGUA RISPOSTA OBBLIGATORIA ###\n{reminder_text}"
        new_system = (
            f"{_LANG_REMINDER_MARKER}\n{_lang_block}\n\n"
            f"{base_system}\n\n{_lang_block}"
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


# Mappa label complexity del classifier LLM -> score 0-100 (universale, no keyword).
_COMPLEXITY_LABEL_SCORE = {"low": 10, "medium": 40, "high": 70}


def compute_iteration_budget(
    prompt: str,
    model: str | None = None,
    classifier_complexity: str | None = None,
    agentic_score: float | None = None,
) -> tuple[int, int]:
    """Calcola il budget di iterazioni per un run agente.

    Ritorna (iter_budget, complexity_score). Il budget e':
        base + per_complexity_point * complexity_score, scalato per weak model,
        capped a max.

    Fonte dello score (WAVE 4, de-lessicalizzazione): se il classifier LLM ha
    prodotto complexity (low/medium/high) e/o agentic_score, lo score viene da
    LI' — universale, indipendente dalla lingua. Solo se il classifier non ha
    fornito nulla si ricade su estimate_prompt_complexity (keyword it/en),
    loggato come lexical_fallback_used.

    Esempi (config default 60/4/300):
        low    (score~10) -> 100 iter ; medium (~40) -> 220 ; high (~70) -> 300.
    """
    config = _load_adaptive_budget_config()
    label = (classifier_complexity or "").strip().lower()
    if label in _COMPLEXITY_LABEL_SCORE:
        score = _COMPLEXITY_LABEL_SCORE[label]
        # Boost dall'agentic_score: un task molto multi-step merita piu' budget.
        if agentic_score is not None:
            score = min(100, score + int(max(0.0, min(1.0, agentic_score)) * 30))
    else:
        score = estimate_prompt_complexity(prompt, config)
        if score > 0:
            logger.info("lexical_fallback_used: compute_iteration_budget keyword complexity")
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
        dburl = _os.environ.get("DATABASE_URL")
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
    """DEPRECATA (2026-06-10): non usare in nuovi call site.

    Euristica keyword sostituita dal punto unico `turn_action_oriented(state)`
    (campo `action_oriented` calcolato da router_node dal classifier LLM sul
    turno corrente). I 5 call site storici sono stati convertiti; questa resta
    solo come riferimento e per i test unitari finche' non vengono migrati.

    Speculare a crates/mcp-core/src/agent_types.rs::detect_action_request.
    """
    if not text or not text.strip():
        return False
    lower = text.lower()
    return any(p in lower for p in _ACTION_PATTERNS)


# ── FIX C: richiesta esplicita di verifica/test da parte dell'utente ────────
# Incident run f1db9550: l'utente ha chiesto "verifica tu stesso provando ad
# accedere con costantino@cobracco.it" ma l'agente ha chiuso senza eseguire
# alcun login/test. Quando il messaggio utente contiene una richiesta esplicita
# di verifica, l'agente DEVE eseguire davvero la verifica (chiamata HTTP/comando)
# e riportarne l'esito reale PRIMA di dichiarare completato. Punto unico:
# rilevatore + iniezione direttiva (riusa il pattern idempotente del lang reminder).
_VERIFICATION_REQUEST_PATTERNS: tuple[str, ...] = (
    # Italiano
    "verifica tu", "verifica che", "verifica il", "verifica la", "verifica se",
    "verifica personalmente", "verifica direttamente", "verificalo",
    "prova ad accedere", "prova a fare", "prova tu", "prova il login",
    "prova a loggarti", "prova a entrare", "provalo", "fai una prova",
    "testa tu", "testa il", "testa la", "testa che", "testalo", "fai un test",
    "assicurati che funzioni", "assicurati che funzionino",
    "controlla che funzioni", "controlla che funzionino",
    "accertati che funzioni", "verifica che funzioni",
    "verifica il funzionamento", "verifica che tutto funzioni",
    # Inglese (l'utente puo' scrivere in inglese: regola lingua a parte)
    "verify yourself", "verify that", "verify it", "test it yourself",
    "try to log in", "try logging in", "try to access", "make sure it works",
    "check that it works", "test that it works", "verify it works",
)


def _detect_verification_request(text: str) -> bool:
    """True se il messaggio utente chiede esplicitamente di verificare/testare.

    Diverso da `_detect_action_request` (che valuta una richiesta d'azione
    generica): qui isoliamo la richiesta di VERIFICA reale (provare il flusso,
    fare login, testare il funzionamento) cosi' l'agente sa che deve eseguire
    la prova e riportarne l'esito, non solo "fare" e dichiarare completato.
    """
    if not text or not text.strip():
        return False
    lower = text.lower()
    return any(p in lower for p in _VERIFICATION_REQUEST_PATTERNS)


# Direttiva di auto-verifica iniettata quando l'utente la richiede (FIX C).
# DB-driven (regola G): testo configurabile, default sicuro se DB down.
_VERIFY_DIRECTIVE_MARKER = "[[NEXUS_VERIFY_DIRECTIVE]]"
_VERIFY_DIRECTIVE_CACHE: dict[str, Any] = {
    "loaded_at": 0.0,
    "enabled": None,
    "text": None,
}
_VERIFY_DIRECTIVE_TTL_SEC = 60.0
_VERIFY_DIRECTIVE_DEFAULT_ENABLED = True
_VERIFY_DIRECTIVE_DEFAULT_TEXT = (
    "L'utente ha chiesto ESPLICITAMENTE di verificare/testare il risultato. "
    "Prima di dichiarare completato DEVI eseguire davvero la verifica con una "
    "tool call concreta (es. run_command con curl/HTTP per provare il login o "
    "l'endpoint, oppure il comando di test pertinente) e riportare l'ESITO "
    "REALE osservato (codice di stato, output, successo/fallimento). NON "
    "inventare ne' assumere il risultato. Se non puoi eseguire la verifica "
    "(strumento mancante, credenziali non disponibili, ambiente non avviabile), "
    "DICHIARALO esplicitamente spiegando cosa manca, invece di dare per scontato "
    "che funzioni."
)


def _load_verification_directive() -> tuple[bool, str]:
    """Legge agent.verification_directive_enabled / _text dal DB (cache 60s).

    Ritorna (enabled, text). Default sicuri se DB down: get_*_setting non
    sollevano mai.
    """
    now = time.time()
    cached = _VERIFY_DIRECTIVE_CACHE["enabled"]
    if cached is not None and (
        now - _VERIFY_DIRECTIVE_CACHE["loaded_at"]
    ) < _VERIFY_DIRECTIVE_TTL_SEC:
        return bool(cached), str(_VERIFY_DIRECTIVE_CACHE["text"])
    enabled = _VERIFY_DIRECTIVE_DEFAULT_ENABLED
    text = _VERIFY_DIRECTIVE_DEFAULT_TEXT
    try:
        from brain.utils.settings_db import get_bool_setting, get_setting
        enabled = get_bool_setting(
            "agent.verification_directive_enabled", _VERIFY_DIRECTIVE_DEFAULT_ENABLED
        )
        text = get_setting(
            "agent.verification_directive_text", _VERIFY_DIRECTIVE_DEFAULT_TEXT
        ).strip() or _VERIFY_DIRECTIVE_DEFAULT_TEXT
    except Exception as exc:
        logger.warning(
            "verification_directive: load DB fallito, uso default (enabled=%s) (%s)",
            _VERIFY_DIRECTIVE_DEFAULT_ENABLED, exc,
        )
    _VERIFY_DIRECTIVE_CACHE["enabled"] = enabled
    _VERIFY_DIRECTIVE_CACHE["text"] = text
    _VERIFY_DIRECTIVE_CACHE["loaded_at"] = now
    return enabled, text


def _inject_verification_directive(
    system_text: str,
    first_human_text: str,
) -> str:
    """Inietta la direttiva di auto-verifica nel system_text se l'utente la chiede.

    Funzione pura e idempotente (marcatore univoco). Iniezione in coda al
    system prompt: la direttiva integra il protocollo agente solo per i run in
    cui l'utente ha chiesto una verifica reale, senza appesantire gli altri.

    Ritorna `system_text` (eventualmente esteso). Invariato se la richiesta non
    contiene una verifica esplicita, se disabilitata, o se gia' presente.
    """
    if not _detect_verification_request(first_human_text):
        return system_text
    enabled, directive = _load_verification_directive()
    if not enabled or not directive:
        return system_text
    base = system_text or ""
    if _VERIFY_DIRECTIVE_MARKER in base:
        return system_text
    block = f"### AUTO-VERIFICA RICHIESTA DALL'UTENTE ###\n{directive}"
    return f"{base}\n\n{_VERIFY_DIRECTIVE_MARKER}\n{block}"


# Pattern di "intenzione imminente non compiuta": il modello annuncia la
# prossima azione (verifica/lettura/esecuzione) ma chiude il turno SENZA
# emettere alcuna tool call. Tipico dei modelli thinking con tool_choice=auto
# (gemini-2.5, deepseek-v4): narrano il piano e si fermano. A differenza di
# _detect_action_request — che valuta la richiesta dell'utente (input) — questo
# valuta l'OUTPUT del modello, cosi' il nudge scatta anche quando il primo
# messaggio umano non e' imperativo (es. "l'applicazione non parte").
_INTENT_NARRATION_PATTERNS: tuple[str, ...] = (
    # Italiano — intenzione futura imminente (verbi d'azione al presente/futuro)
    "inizio verificando", "inizio controllando", "inizio analizzando",
    "inizio leggendo", "inizio esaminando", "inizio con ", "inizio a ",
    "inizio dal", "inizio dalla", "comincio con", "comincio a ",
    "comincio verificando", "comincio dal", "iniziamo verificando",
    "iniziamo con", "iniziamo dal", "cominciamo con", "partiamo da",
    "procedo a ", "procedo con", "procedo alla", "procedo nel", "procedo ora",
    "procedo subito", "procediamo con", "vado a ", "ora vado", "adesso vado",
    "ora verifico", "ora controllo", "ora leggo", "ora analizzo", "ora eseguo",
    "ora apro", "ora esamino", "adesso verifico", "adesso controllo",
    "adesso leggo", "verifico la presenza", "verifico se", "verifico il",
    "verifico la config", "verifico ora", "controllo la presenza",
    "controllo se", "controllo il", "controllo la config", "esamino il",
    "esamino la", "leggo il", "leggo la config", "fammi verificare",
    "fammi controllare", "fammi leggere", "fammi dare un", "fammi guardare",
    "il prossimo passo", "prossimo step", "passo successivo", "passo a ",
    "proseguo con", "proseguo a ",
    # Italiano — gerundio "sto + gerundio" (Fix A ADR 0017, caso chat 6 reale
    # Beauty-Book run e38aaba7: "Sto procedendo con la creazione di altri test").
    # I pattern "procedo con" sopra NON matchavano "procedENDO con": gap colmato.
    "sto procedendo", "procedendo con", "procedendo a ", "procedendo alla",
    "sto creando", "sto implementando", "sto scrivendo", "sto aggiungendo",
    "sto generando", "sto preparando", "sto sviluppando",
    "stiamo procedendo", "stiamo creando", "stiamo implementando",
    # Italiano — futuro semplice (annuncio: "creero il file X" -> mai eseguito)
    "creerò ", "creero ", "implementerò ", "implementero ",
    "scriverò ", "scrivero ", "aggiungerò ", "aggiungero ",
    "genererò ", "generero ", "preparerò ", "preparero ",
    "continuerò ", "continuero ", "proseguirò ", "proseguiro ",
    "il prossimo file", "i prossimi file", "i prossimi test",
    # Italiano — perifrasi "continuo con" / "passo al"
    "continuo con", "continuo a ", "passo al", "passo alla", "passo ai",
    "ora creo", "ora implemento", "ora scrivo", "ora aggiungo",
    "adesso creo", "adesso implemento", "adesso scrivo",
    # Inglese — intenzione futura imminente
    "let me check", "let me verify", "let me start", "let me read",
    "let me look", "let me inspect", "let me examine", "let me first",
    "let me begin", "i'll check", "i'll verify", "i'll start", "i'll read",
    "i'll look", "i'll first", "i'll begin", "i'll inspect", "i'll examine",
    "i will check", "i will verify", "i will start", "i will read",
    "i'm going to", "i am going to", "let's check", "let's verify",
    "let's start", "let's look", "next, i", "now i'll", "now i will",
    "first, i'll", "first i'll", "first, let me",
    # Inglese — present progressive + future complementari (gap colmato)
    "i'm proceeding", "i am proceeding", "i'll proceed", "i will proceed",
    "i'm creating", "i'm implementing", "i'm writing", "i'm adding",
    "moving on to", "continuing with", "next i will create",
    "i'll create", "i'll implement", "i'll write", "i'll add",
    "i will create", "i will implement", "i will write", "i will add",
    "the next step is", "the next file",
    # Italiano — POLLING/ATTESA: l'agente temporeggia ("attendo e ricontrollo")
    # invece di diagnosticare. Caso Beauty-Book run 2026-06-07: "Attendo qualche
    # istante e verifico di nuovo" -> end_turn senza azione, container in
    # crash-loop mai diagnosticato. Senza questi pattern il segnale unfulfilled
    # non scattava (presente indicativo, non futuro/gerundio).
    "attendo ", "attendo qualche", "attendo ancora", "attendo che",
    "attendo il", "attendo un", "aspetto ", "aspetto che", "aspetto qualche",
    "aspetto ancora", "ricontrollo", "ricontrollare", "verifico di nuovo",
    "controllo di nuovo", "verifico nuovamente", "controllo nuovamente",
    "riprovo tra", "riprovo a ", "riprovo subito", "riprovo ora",
    "provo di nuovo", "provo ancora",
    # Inglese — polling/attesa
    "i'll check again", "let me check again", "i'll wait", "let me wait",
    "waiting for", "i'll retry", "let me retry", "checking again",
    "i'll verify again", "i'll re-check", "let me re-check", "i'll try again",
)

# Sottoinsieme POLLING: l'agente vuole solo "ri-controllare/aspettare" lo stato
# (tipico dei wait-loop su container/servizi che non partono). Usato per il
# nudge anti wait-loop: invece di ri-attendere, l'agente deve DIAGNOSTICARE.
_POLLING_WAIT_PATTERNS: tuple[str, ...] = (
    "attendo ", "attendo qualche", "attendo ancora", "attendo che",
    "attendo il", "attendo un", "aspetto ", "aspetto che", "aspetto qualche",
    "aspetto ancora", "ricontrollo", "ricontrollare", "verifico di nuovo",
    "controllo di nuovo", "verifico nuovamente", "controllo nuovamente",
    "riprovo tra", "riprovo subito", "riprovo ora", "provo di nuovo",
    "provo ancora", "i'll check again", "let me check again", "i'll wait",
    "let me wait", "waiting for", "i'll retry", "checking again",
    "i'll verify again", "i'll re-check", "i'll try again",
)

# Rilevamento MORFOLOGICO (regola H: robusto a verbi nuovi, evita la blacklist
# che cresce a ogni caso). Complementa _INTENT_NARRATION_PATTERNS catturando:
#   1. Futuro 1a persona italiano: qualunque verbo terminante in "-rò" accentato
#      (creerò, estrarrò, scomporrò, dividerò, sposterò, rifattorizzerò, farò,
#      andrò...). Cosi' non serve elencare ogni verbo. Unico falso positivo
#      frequente escluso: "però".
#   2. Trigger d'avvio + gerundio ("inizio creando", "sto procedendo", "ora
#      generando"): il gerundio "-ndo" dopo un avverbio/verbo d'avvio segnala
#      un'azione narrata e non ancora eseguita.
# I falsi positivi residui costano solo un re-route, limitato dal cap
# g1_reroute_count; un falso negativo invece lascia il task a meta'.
_FUTURE_1P_RE = re.compile(r"\b(?!però\b)\w{2,}rò\b")
_START_GERUND_RE = re.compile(
    r"\b(inizio|comincio|sto|stiamo|iniziamo|cominciamo|ora|adesso|poi|quindi|"
    r"prima|dopo)\s+\w*ndo\b"
)


def _detect_unfulfilled_intent(text: str | None) -> bool:
    """True se l'OUTPUT annuncia un'azione imminente ma non l'ha eseguita.

    Valutato sulla CODA del testo (la narrazione di intenzione chiude
    tipicamente il messaggio: "...Inizio verificando index.html."). Usato dal
    router G1 e dal nudge executor per ri-mandare all'executor quando un modello
    thinking narra il piano e chiude senza tool call. Il cap g1_reroute_count
    impedisce loop infiniti su eventuali falsi positivi.

    Due livelli: la blacklist storica _INTENT_NARRATION_PATTERNS (frasi precise,
    inclusi i casi inglesi) piu' il rilevamento morfologico (_FUTURE_1P_RE /
    _START_GERUND_RE) che generalizza i futuri/gerundi italiani senza inseguire
    ogni nuovo verbo.
    """
    if not text or not text.strip():
        return False
    tail = text.strip().lower()[-400:]
    if any(p in tail for p in _INTENT_NARRATION_PATTERNS):
        return True
    return bool(_FUTURE_1P_RE.search(tail) or _START_GERUND_RE.search(tail))


def _detect_polling_wait(text: str | None) -> bool:
    """True se l'OUTPUT e' un'attesa/polling passiva ("attendo e ricontrollo").

    Distingue il wait-loop (l'agente temporeggia su uno stato che non cambia, es.
    container in crash-loop) dall'intenzione d'azione normale. Usato per il nudge
    anti wait-loop: invece di ri-attendere, l'agente deve DIAGNOSTICARE. Valutato
    sulla coda del testo (l'attesa chiude tipicamente il messaggio).
    """
    if not text or not text.strip():
        return False
    tail = text.strip().lower()[-400:]
    return any(p in tail for p in _POLLING_WAIT_PATTERNS)


def build_unfulfilled_report(result_text: str | None, messages: list | None) -> str:
    """Resoconto onesto quando il turno chiude con un'intenzione non eseguita e
    NON si fa auto-restart (modalita' confirm o cap raggiunto).

    Deterministico (nessuna chiamata LLM, niente nuova allucinazione): sintetizza
    le azioni gia' svolte dalla history (tool usati + file toccati), dichiara lo
    stato (interrotto, non completato) e propone il prossimo passo. Sostituisce la
    "promessa monca" come final answer, cosi' l'utente riceve un resoconto invece
    di "attendo e verifico di nuovo".
    """
    tool_counts: dict[str, int] = {}
    files_touched: list[str] = []
    for m in messages or []:
        content = getattr(m, "content", None)
        if not isinstance(content, list):
            continue
        for block in content:
            if not isinstance(block, dict):
                continue
            if block.get("type") == "tool_use":
                name = str(block.get("name") or "tool")
                tool_counts[name] = tool_counts.get(name, 0) + 1
                inp = block.get("input")
                if isinstance(inp, dict):
                    path = inp.get("path") or inp.get("file_path") or inp.get("filename")
                    if isinstance(path, str) and path and path not in files_touched:
                        files_touched.append(path)
    lines: list[str] = [
        "Mi sono fermato annunciando un'attesa o un passo successivo senza "
        "eseguirlo, quindi il compito NON e' completato. Ecco il resoconto onesto:",
        "",
    ]
    if tool_counts:
        azioni = ", ".join(
            f"{n}x {name}" for name, n in sorted(tool_counts.items(), key=lambda kv: -kv[1])
        )
        lines.append(f"- Cosa ho fatto: {azioni}.")
    else:
        lines.append("- Cosa ho fatto: nessuna azione concreta in questo turno.")
    if files_touched:
        shown = ", ".join(files_touched[:12])
        more = "" if len(files_touched) <= 12 else f" (+{len(files_touched) - 12} altri)"
        lines.append(f"- File toccati: {shown}{more}.")
    snippet = (result_text or "").strip().replace("\n", " ")
    if snippet:
        lines.append(f'- Dove mi sono interrotto: "{snippet[-180:]}"')
    lines.append(
        "- Cosa manca: portare a termine il compito; l'ultimo passo annunciato "
        "non e' stato eseguito."
    )
    lines.append(
        "- Prossimo passo proposto: invece di attendere passivamente, diagnosticare "
        "lo stato reale (es. leggere i log del servizio/container che non parte) e "
        "agire sulla causa. Confermi se procedo?"
    )
    return "\n".join(lines)


def _last_assistant_text(messages: list) -> str:
    """Ritorna il testo dell'ultimo AIMessage nella history (vuoto se assente)."""
    for m in reversed(messages or []):
        if isinstance(m, AIMessage):
            content = getattr(m, "content", "")
            if isinstance(content, list):
                return " ".join(
                    b.get("text", "") for b in content
                    if isinstance(b, dict) and b.get("type") == "text"
                )
            return str(content or "")
    return ""


def _pick_escalation_model(
    provider: str | None, model: str | None, escalations: int
) -> tuple[str, str] | None:
    """Sceglie un modello piu' capace per l'escalation dell'orchestratore.

    Priorita' (nessun fallback hardcoded, regola G):
      1. Catena intra-provider DB (nexus_model_escalation_chain): stesso
         provider, tier superiore, posizione = numero di escalation gia' fatte.
      2. Purpose model cross-provider (loop_fallback_default) dal router.

    Ritorna (provider, model) del modello escalato, o None se non c'e' un
    candidato diverso da quello corrente. Usato sia dalla loop-detection sia
    dal cap G1 (modello che descrive senza agire): in entrambi i casi
    l'orchestratore promuove il turno a un modello migliore invece di arrendersi.
    """
    # Cooldown gate (ADR 0020): consulta la fonte di verita' unica (gate Rust)
    # PRIMA di scegliere. La catena intra-provider (Tier 1) resta sullo stesso
    # provider: se quel provider e' in cooldown billing/quota, escalare su di
    # lui sprecherebbe un turno (incidente reale). In quel caso saltiamo Tier 1
    # e andiamo al Tier 2 cross-provider, che il gate filtra gia'.
    cooldown_set: set[str] = set()
    try:
        from brain.router.service import _routing_client_singleton
        _cd = _routing_client_singleton().cooldown_providers()
        if _cd is not None:
            cooldown_set = _cd
    except Exception:
        cooldown_set = set()

    # Tier 1: catena intra-provider (stesso provider, tier superiore).
    if provider and model and provider.strip().lower() not in cooldown_set:
        try:
            import psycopg2  # type: ignore[import]
            import os as _os
            _db_url = _get_db_url()
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
                _cand = _rows[escalations][0]
                if _cand and _cand != model:
                    return (provider, _cand)
        except Exception as _e:
            logger.debug("_pick_escalation_model: catena DB fallita: %s", _e)
    # Tier 2: purpose model cross-provider dal router Rust.
    try:
        from brain.router.service import _routing_client_singleton
        d = _routing_client_singleton().purpose_model(purpose="loop_fallback_default")
        if (
            d.provider not in ("__router_unavailable__", "__no_capable_provider__")
            and not (d.provider == provider and d.model == model)
        ):
            return (d.provider, d.model)
    except Exception:
        pass
    return None


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


def has_productive_action_in_history(messages: list) -> bool:
    """True se il run ha gia' eseguito almeno UN'azione PRODUTTIVA (tool_use con
    nome NON in _EXPLORATION_ONLY_TOOLS: write_file, edit_file, run_command, ...).

    Punto unico (regola L) del fatto strutturale "questo run ha gia' agito".
    Usato dal routing G1: se il run ha gia' prodotto azioni, una chiusura
    end_turn testuale e' il RESOCONTO FINALE legittimo del lavoro svolto, NON
    una "risposta descrittiva senza azione" — ri-mandarla all'executor produceva
    reroute G1 a vuoto su lavoro gia' concluso, escalation inutile di modello e
    infine il cap-text contraddittorio appeso in coda a una risposta conclusa
    ("Modello non risponde con azione..." dopo "Considero l'intervento
    concluso."). Fatto strutturale, nessuna analisi lessicale del testo.
    """
    for m in messages:
        if not isinstance(m, AIMessage):
            continue
        extra = getattr(m, "additional_kwargs", {}) or {}
        blocks = extra.get("anthropic_content") or []
        if not isinstance(blocks, list):
            continue
        for b in blocks:
            if (
                isinstance(b, dict)
                and b.get("type") == "tool_use"
                and (b.get("name") or "") not in _EXPLORATION_ONLY_TOOLS
            ):
                return True
    return False


# Default IDENTICO a MUTATORS_DEFAULT in crates/mcp-core/src/agent_tool_result_cache.rs
# (mig 0394). Il punto unico dei DATI e' il setting DB condiviso; questo default
# serve solo se la chiave manca o il DB e' irraggiungibile.
_FS_MUTATORS_DEFAULT = (
    "write_file,edit_file,delete_file,rename_file,file_write,fs_copy,fs_mkdir,"
    "fs_move,format_file,run_lint_fix,run_command,command,run_in_terminal,"
    "git_command,git_pull,git_commit,git_stage,git_push,nexus_extract_figma_code,"
    "nexus_install_shadcn_components,nexus_mcp_tool_call,cargo_install,run_service,"
    "service_restart,stop_service"
)
_fs_mutators_cache: frozenset[str] | None = None
_fs_mutators_ts: float = 0.0


def _load_fs_mutator_tools() -> frozenset[str]:
    """Tool che MUTANO filesystem/progetto. Punto unico dei dati: setting
    `agent.tools.result_cache_mutators` (mig 0394), condiviso con la cache
    tool_result lato Rust. Cache 60s; fail-safe sul default."""
    global _fs_mutators_cache, _fs_mutators_ts
    import time as _time

    now = _time.monotonic()
    if _fs_mutators_cache is not None and now - _fs_mutators_ts < 60.0:
        return _fs_mutators_cache
    try:
        from brain.utils import settings_db
        csv = settings_db.get_setting(
            "agent.tools.result_cache_mutators", _FS_MUTATORS_DEFAULT
        ) or _FS_MUTATORS_DEFAULT
    except Exception:
        csv = _FS_MUTATORS_DEFAULT
    out = frozenset(s.strip() for s in csv.split(",") if s.strip())
    _fs_mutators_cache, _fs_mutators_ts = out, now
    return out


def has_filesystem_mutation_in_history(messages: list) -> bool:
    """True se il run ha eseguito almeno un tool che MUTA filesystem/progetto
    (write/edit/rename/estrazioni/comandi). Fatto STRUTTURALE, nessuna analisi
    lessicale. Usato dall'eleggibilita' del final_gate: un run che ha toccato il
    progetto va verificato a prescindere dall'intent classificato (il caso reale
    era intent=architecture — fuori dalla whitelist software_intents — che pero'
    aveva spostato file con rename: chiusura senza alcuna verifica)."""
    mutators = _load_fs_mutator_tools()
    for m in messages:
        if not isinstance(m, AIMessage):
            continue
        extra = getattr(m, "additional_kwargs", {}) or {}
        blocks = extra.get("anthropic_content") or []
        if not isinstance(blocks, list):
            continue
        for b in blocks:
            if (
                isinstance(b, dict)
                and b.get("type") == "tool_use"
                and (b.get("name") or "") in mutators
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


# ── Cache soglia azioni produttive ripetute identiche (TTL 60s) ─────────────
# FIX B (incident run f1db9550): il signature-loop esistente cattura SOLO la
# stessa identica tool call ripetuta >=3 volte in una finestra di 6 signature.
# Una SEQUENZA di azioni diverse (es. edit_file -> npm install -> rm node_modules
# -> npm install -> npm run build) ripetuta integralmente 2 volte non scatta: ogni
# singola azione appare 2 volte, sotto la soglia 3. Qui rileviamo la ripetizione
# IDENTICA di una azione PRODUTTIVA (scrittura/comando), indipendentemente
# dall'esito (diverso da _detect_repeated_failed_command, che richiede errore).
# Soglia DB-driven (regola G): default 2 (la seconda ripetizione identica e'
# gia' sintomo di stallo su lavoro reale, non vale la pena ripetere una terza).
_REPEATED_ACTION_CACHE: dict[str, Any] = {"loaded_at": 0.0, "threshold": None}
_REPEATED_ACTION_TTL_SEC = 60.0
_REPEATED_ACTION_DEFAULT_THRESHOLD = 2

# Tool PRODUTTIVI tracciati per azione ripetuta -> chiave argomento che ne
# definisce l'identita'. Una ripetizione identica = stesso tool + stesso valore
# della chiave (path per le scritture, command per i comandi).
_REPEATED_ACTION_TOOLS: dict[str, tuple[str, ...]] = {
    "write_file": ("path", "file_path"),
    "edit_file": ("path", "file_path"),
    "run_command": ("command",),
    "run_service": ("command",),
    "run_in_terminal": ("command",),
}


def _load_repeated_action_threshold() -> int:
    """Legge agent.repeated_action_threshold dal DB con cache 60s.

    Ritorna il default sicuro (2) se il DB e' irraggiungibile o la chiave non
    esiste: get_int_setting non solleva mai. Soglia minima 2 (1 sarebbe la
    prima esecuzione legittima, non una ripetizione).
    """
    now = time.time()
    cached = _REPEATED_ACTION_CACHE["threshold"]
    if cached is not None and (
        now - _REPEATED_ACTION_CACHE["loaded_at"]
    ) < _REPEATED_ACTION_TTL_SEC:
        return int(cached)
    value = _REPEATED_ACTION_DEFAULT_THRESHOLD
    try:
        from brain.utils.settings_db import get_int_setting
        value = int(get_int_setting(
            "agent.repeated_action_threshold", _REPEATED_ACTION_DEFAULT_THRESHOLD
        ))
        if value < 2:
            value = _REPEATED_ACTION_DEFAULT_THRESHOLD
    except Exception as exc:
        logger.warning(
            "repeated_action_threshold: load DB fallito, uso default %d (%s)",
            _REPEATED_ACTION_DEFAULT_THRESHOLD, exc,
        )
        value = _REPEATED_ACTION_DEFAULT_THRESHOLD
    _REPEATED_ACTION_CACHE["threshold"] = value
    _REPEATED_ACTION_CACHE["loaded_at"] = now
    return value


def _tool_result_outcome_after(recent: list, idx: int, max_ahead: int = 3) -> bool | None:
    """Esito del primo tool_result nei `max_ahead` messaggi dopo recent[idx].

    Ritorna True=errore, False=successo, None=nessun risultato trovato (es.
    tool_use ancora pending in coda alla history). Gestisce ENTRAMBI i formati
    di tool_result presenti nel grafo: ToolMessage (formato langchain classico)
    e HumanMessage con anthropic_content=[{type: tool_result, ...}] (formato
    emesso da tool_dispatch_node). Punto unico (regola L) della domanda
    "il tool_use a recent[idx] e' riuscito?".

    Gerarchia dei segnali (contratto dati A, censimento 2026-06-10):
      1. exit_code STRUTTURATO (tool-comando): 0=successo, !=0=errore. Certo.
      2. is_error STRUTTURATO del blocco/ToolMessage.
      3. _TOOL_ERROR_HINTS sul testo: SOLO fallback, loggato come
         lexical_fallback_used per misurarne la residualita'.
    """
    for j in range(idx + 1, min(idx + 1 + max_ahead, len(recent))):
        nm = recent[j]
        if isinstance(nm, ToolMessage):
            if getattr(nm, "status", "") == "error":
                return True
            c = getattr(nm, "content", "")
            if isinstance(c, list):
                for cc in c:
                    if isinstance(cc, dict):
                        if cc.get("is_error"):
                            return True
                        txt = str(cc.get("text", "") or cc.get("content", ""))
                        if any(h in txt.lower() for h in _TOOL_ERROR_HINTS):
                            logger.info("lexical_fallback_used: _tool_result_outcome_after/ToolMessage")
                            return True
                return False
            if any(h in str(c).lower() for h in _TOOL_ERROR_HINTS):
                logger.info("lexical_fallback_used: _tool_result_outcome_after/ToolMessage-str")
                return True
            return False
        if isinstance(nm, HumanMessage):
            extra = getattr(nm, "additional_kwargs", {}) or {}
            blocks = extra.get("anthropic_content") or []
            found_result = False
            for bb in blocks if isinstance(blocks, list) else []:
                if not isinstance(bb, dict) or bb.get("type") != "tool_result":
                    continue
                found_result = True
                # 1) Segnale strutturale primario: exit_code (tool-comando).
                ec = bb.get("exit_code")
                if isinstance(ec, int):
                    return ec != 0
                # 2) is_error strutturato.
                if bb.get("is_error"):
                    return True
                # 3) Fallback lessicale sul testo (loggato).
                cont = bb.get("content")
                txts: list[str] = []
                if isinstance(cont, list):
                    for cc in cont:
                        if isinstance(cc, dict):
                            txts.append(str(cc.get("text", "") or cc.get("content", "")))
                elif cont is not None:
                    txts.append(str(cont))
                if any(h in t.lower() for t in txts for h in _TOOL_ERROR_HINTS):
                    logger.info("lexical_fallback_used: _tool_result_outcome_after/anthropic_content")
                    return True
            if found_result:
                return False
    return None


def _detect_repeated_action(messages: list, lookback: int = 24) -> tuple[str | None, int]:
    """Rileva la ripetizione IDENTICA di una azione produttiva (scrittura/comando).

    Diverso da `_detect_repeated_failed_command`: NON richiede che l'azione sia
    fallita. Cattura il pattern "stessa azione produttiva eseguita N volte" che
    indica uno stallo su lavoro reale (incident run f1db9550: stessa sequenza
    edit_file/npm install/build ripetuta integralmente).

    Scansiona gli ultimi `lookback` messaggi, estrae i blocchi tool_use dei tool
    in `_REPEATED_ACTION_TOOLS` e costruisce una signature `name|valore-chiave`.
    Conta le occorrenze di ogni signature.

    FALSO-DOPPIONE (incidente 2026-06-10, due run buoni chiusi failed): una
    signature la cui PRIMA occorrenza e' RIUSCITA non e' uno stallo. Il pattern
    reale era: edit_file applicato con successo -> il modello ri-emette lo stesso
    edit -> fallisce con "old_string non trovato" (perche' GIA' applicato) ->
    count=2 -> abort "bloccato ripetendo la stessa azione" su un run COMPLETO.
    Le signature con almeno un esito di successo vengono quindi escluse dal
    conteggio: la ri-esecuzione di un'azione gia' applicata e' ridondanza
    innocua (il tool_result d'errore informa il modello), non uno stallo. Lo
    stallo vero (stessa azione che NON riesce mai) resta rilevato.

    Ritorna `(label, count)` della signature piu' frequente (label leggibile
    "name: valore"), oppure `(None, 0)` se nessuna azione tracciata e' presente.
    """
    if not messages:
        return (None, 0)
    counts: dict[str, int] = {}
    labels: dict[str, str] = {}
    succeeded: set[str] = set()
    last_sig: str | None = None
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
            keys = _REPEATED_ACTION_TOOLS.get(name)
            if not keys:
                continue
            inp = b.get("input", {}) or {}
            value = ""
            for k in keys:
                v = str(inp.get(k, "") or "").strip()
                if v:
                    value = v
                    break
            if not value:
                continue
            sig = f"{name}|{value}"
            counts[sig] = counts.get(sig, 0) + 1
            labels[sig] = f"{name}: {value[:120]}"
            last_sig = sig
            # Esito strutturale: un successo marca la signature come
            # "lavoro gia' riuscito" -> mai stallo da abort.
            if _tool_result_outcome_after(recent, idx) is False:
                succeeded.add(sig)
    for sig in succeeded:
        counts.pop(sig, None)
    if not counts:
        return (None, 0)
    top = max(counts.items(), key=lambda kv: (kv[1], kv[0] == last_sig))
    return (labels.get(top[0], top[0]), top[1])


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
        db_url = get_db_url()
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


# ── Tokenizer-based context size (ADR 0016 Fase D) ──────────────────────────
# Sostituisce la stima `chars//4` (sottostima fino a 3-5x su contenuti densi:
# JSON tool_result, codice, CJK, base64). tiktoken cl100k_base e' lo stesso
# tokenizer BPE usato da Claude/GPT/Mistral. Cache LRU su hash content per
# evitare ri-tokenizzazione di messaggi gia' visti (perf critica con history
# lunga). Fallback a chars//4 se tiktoken non disponibile (regola H: niente
# panic, degrada in modo controllato).
import functools as _functools_for_tok
import hashlib as _hashlib_for_tok

_TOKENIZER_CACHE: dict[str, Any] = {"encoder": None, "name": None}
_TOK_DEFAULT_NAME = "cl100k_base"


def _get_tokenizer():
    """Carica e cache l'encoder tiktoken (key: agent.context.tokenizer)."""
    try:
        from brain.utils.settings_db import get_setting
        name = (get_setting("agent.context.tokenizer", _TOK_DEFAULT_NAME) or _TOK_DEFAULT_NAME).strip()
    except Exception:
        name = _TOK_DEFAULT_NAME
    if _TOKENIZER_CACHE["encoder"] is not None and _TOKENIZER_CACHE["name"] == name:
        return _TOKENIZER_CACHE["encoder"]
    try:
        import tiktoken
        enc = tiktoken.get_encoding(name)
    except Exception as exc:
        logger.warning("tokenizer %s non disponibile (%s), fallback chars//4", name, exc)
        return None
    _TOKENIZER_CACHE["encoder"] = enc
    _TOKENIZER_CACHE["name"] = name
    return enc


@_functools_for_tok.lru_cache(maxsize=4096)
def _count_tokens_cached(content_hash: str, length: int) -> int:
    """Slot LRU: il vero conteggio passa per _count_tokens (che chiama questa)."""
    # NB: questa funzione e' un placeholder per cache: la chiave include hash+len
    # per evitare collisioni. Il valore reale viene messo via _count_tokens.
    return length // 4  # fallback se mai raggiunto direttamente


def _count_tokens(text: str) -> int:
    """Conta token di una stringa con tiktoken + cache LRU per hash."""
    if not text:
        return 0
    enc = _get_tokenizer()
    if enc is None:
        return max(1, len(text) // 4)
    # Cache: chiave hash content (i messaggi vecchi sono identici tra turni).
    h = _hashlib_for_tok.sha1(text.encode("utf-8", "ignore")).hexdigest()
    cache_key = (h, len(text))
    cached = _COUNT_TOKENS_CACHE.get(cache_key)
    if cached is not None:
        return cached
    try:
        n = len(enc.encode(text, disallowed_special=()))
    except Exception:
        n = max(1, len(text) // 4)
    _COUNT_TOKENS_CACHE[cache_key] = n
    # Cap dimensione cache LRU semplice.
    if len(_COUNT_TOKENS_CACHE) > 4096:
        # rimuovi il piu' vecchio (FIFO approssimato — dict mantiene insertion order)
        first_key = next(iter(_COUNT_TOKENS_CACHE))
        _COUNT_TOKENS_CACHE.pop(first_key, None)
    return n


_COUNT_TOKENS_CACHE: dict[tuple[str, int], int] = {}


# ── Rolling summary cross-turno (ADR 0016 Fase A.3) ─────────────────────────
# Ogni N turni, sostituisce i messaggi piu' vecchi con un summary compatto.
# Gli originali restano in Qdrant (collection chat_history_rolling) e sono
# retrievabili con nexus_search_semantic(source_kinds=chat_history).
# Implementazione MINIMA: tronca i messaggi vecchi a `aggressive_max_chars`
# preservando role+content essenziale, e li offloada in batch.
_ROLLING_DEFAULT_ENABLED = True
_ROLLING_DEFAULT_WINDOW = 5
_ROLLING_DEFAULT_KEEP_RECENT = 3


def _load_rolling_config() -> dict[str, Any]:
    """Settings rolling summary (cache via _CTX_MGMT* esistente)."""
    enabled = _ROLLING_DEFAULT_ENABLED
    window = _ROLLING_DEFAULT_WINDOW
    keep_recent = _ROLLING_DEFAULT_KEEP_RECENT
    try:
        from brain.utils.settings_db import get_bool_setting, get_setting
        enabled = get_bool_setting("agent.context.rolling_summary_enabled", True)
        window = int(get_setting("agent.context.rolling_window_turns", "5") or "5")
        keep_recent = int(get_setting("agent.context.rolling_keep_recent_turns", "3") or "3")
    except Exception as exc:
        logger.debug("rolling_summary: load DB fallito (%s), default", exc)
    return {"enabled": enabled, "window": window, "keep_recent": keep_recent}


def _apply_rolling_summary(messages: list[Any], iteration: int, embeddings: Any | None) -> list[Any]:
    """Ogni `window` turni, offloada i messaggi piu' vecchi e li sostituisce
    con un placeholder compatto. Mantiene `keep_recent` integri.

    Best-effort: non solleva mai. Quando l'offload non e' disponibile,
    semplicemente non comprime (degrada ai brake successivi).
    """
    cfg = _load_rolling_config()
    if not cfg["enabled"]:
        return messages
    if iteration <= 0 or iteration % cfg["window"] != 0:
        return messages
    if len(messages) <= cfg["keep_recent"] + 1:
        return messages

    try:
        from brain.agents import context_offload
        first_human = _first_human_index(messages)
        cutoff = max(first_human + 1, len(messages) - cfg["keep_recent"])
        if cutoff <= 1:
            return messages
        # Snap del cutoff in avanti: se messages[cutoff] e' un ToolMessage, il
        # suo AIMessage(tool_calls=...) corrispondente e' finito in to_compress
        # -> il ToolMessage resterebbe ORFANO nella history mantenuta. Mistral,
        # DeepSeek, OpenAI rifiutano lo schema con "Unexpected role" / "Messages
        # with role 'tool' must follow a message with role 'assistant' that has
        # tool_calls" (causa del bug osservato in prod su rolling_summary). Lo
        # avanziamo finche' non e' piu' un tool message. Stesso fix anche per
        # AIMessage con tool_calls i cui tool response cadrebbero in to_compress
        # (simmetrico: AIMessage(tool_calls) senza ToolMessage che lo segue).
        max_cutoff = len(messages) - 1  # lascia almeno un messaggio recente
        while cutoff < max_cutoff and getattr(messages[cutoff], "type", None) == "tool":
            cutoff += 1
        if cutoff <= 1:
            return messages
        to_compress = messages[1:cutoff] if first_human == 0 else messages[:cutoff]
        if not to_compress:
            return messages

        # Concatena i contenuti per offload (manteniamo struttura semantica
        # role:content separati da delimitatori per retrieval).
        parts: list[str] = []
        for m in to_compress:
            role = m.__class__.__name__
            content = getattr(m, "content", None)
            content_str = content if isinstance(content, str) else json.dumps(content, ensure_ascii=False) if content else ""
            parts.append(f"### {role}\n{content_str}")
        full_text = "\n\n".join(parts)
        if len(full_text) < 2000:
            return messages  # troppo piccolo, non vale la pena

        offload = context_offload.offload_to_rag(
            embeddings,
            full_text,
            source_kind="chat_history",
            metadata={"turns_compressed": len(to_compress), "at_iteration": iteration},
        )
        pointer = context_offload.build_pointer(
            len(full_text), offload, what="messaggi precedenti"
        )
        # Sostituisci il blocco con UN messaggio di summary.
        # IMPORTANTE: usiamo HumanMessage, NON SystemMessage. Causa root del bug
        # "Unexpected role" / "Messages with role" osservato su mistral/deepseek/
        # openai: un SystemMessage in posizione NON iniziale (qui veniva inserito
        # dopo HumanMessage[0]: vedi rami first_human==0 sotto) viola lo schema
        # OpenAI-compatible che ammette un solo SystemMessage all'inizio. Allinea
        # con il punto unico Rust db_role_to_llm_role (chat_messages::context):
        # ogni summary interno -> ruolo 'user' verso i provider. Il prefisso
        # "[Rolling summary ...]" rende riconoscibile la natura del messaggio nel
        # _is_summary_message (vedi sotto) e nei prompt downstream.
        try:
            from langchain_core.messages import HumanMessage
            summary_msg = HumanMessage(content=(
                f"[Rolling summary turni 1..{cutoff}]\n"
                f"{len(to_compress)} messaggi precedenti compressi e indicizzati. {pointer}"
            ))
        except Exception:
            summary_msg = to_compress[-1]  # fallback minimo
        new_messages = [summary_msg] + messages[cutoff:]
        if first_human == 0 and len(messages) > 0:
            new_messages = [messages[0], summary_msg] + messages[cutoff:]
        logger.info(
            "rolling_summary: iter=%d offload %d messaggi (%d char) -> 1 summary",
            iteration, len(to_compress), len(full_text),
        )
        return new_messages
    except Exception as exc:
        logger.warning("rolling_summary fallito (best-effort): %s", exc)
        return messages


# ── System prompt offload (ADR 0016 Fase A.1) ───────────────────────────────
# Quando il system prompt (project context, KB snapshot, tool defs verbose)
# supera `agent.context.system_prompt_offload_threshold_tokens`, il blocco viene
# indicizzato in Qdrant (collection tool_results_chunks, source_kind=system_context)
# e nel prompt resta un summary + pointer esplicito a `nexus_search_semantic`.
# Riusa `offload_to_rag` esistente: l'agente recupera con il tool gia' noto.
_SYS_OFFLOAD_DEFAULT_THRESHOLD = 8000
_SYS_OFFLOAD_DEFAULT_SUMMARY_MAX = 800


def _load_system_offload_config() -> tuple[int, int]:
    """Threshold token + max summary token (cache 60s)."""
    threshold = _SYS_OFFLOAD_DEFAULT_THRESHOLD
    max_summary = _SYS_OFFLOAD_DEFAULT_SUMMARY_MAX
    try:
        from brain.utils.settings_db import get_setting
        threshold = int(
            get_setting(
                "agent.context.system_prompt_offload_threshold_tokens",
                str(_SYS_OFFLOAD_DEFAULT_THRESHOLD),
            )
        )
        max_summary = int(
            get_setting(
                "agent.context.system_prompt_summary_max_tokens",
                str(_SYS_OFFLOAD_DEFAULT_SUMMARY_MAX),
            )
        )
    except Exception as exc:
        logger.debug("system_offload: load DB fallito, default (%s)", exc)
    return threshold, max_summary


def _offload_system_prompt_if_huge(system_text: str, embeddings: Any | None) -> str:
    """Se `system_text` > threshold token, offload in Qdrant e ritorna summary+pointer.

    Mantiene SEMPRE l'intestazione del system prompt (prime righe, fino a
    `summary_max_tokens` stimati). Il resto va in Qdrant ed e' raggiungibile
    via `nexus_search_semantic(source_kinds=system_context)`. Best-effort:
    in caso di errore offload, ritorna il system_text invariato.
    """
    if not system_text:
        return system_text
    threshold_tokens, max_summary_tokens = _load_system_offload_config()
    est_tokens = _count_tokens(system_text)
    if est_tokens <= threshold_tokens:
        return system_text

    # Calcola char budget per il summary: ~4 char/token + 200 per pointer.
    summary_chars = max_summary_tokens * 4
    head = system_text[:summary_chars]
    try:
        from brain.agents import context_offload
        offload = context_offload.offload_to_rag(
            embeddings,
            system_text,
            source_kind="system_context",
            metadata={"est_tokens": est_tokens},
        )
        pointer = context_offload.build_pointer(
            len(system_text), offload, what="system prompt"
        )
        logger.info(
            "system_prompt_offload: est_tokens=%d > threshold=%d -> offloadato "
            "(head=%d char + pointer)",
            est_tokens, threshold_tokens, len(head),
        )
        return f"{head}\n\n{pointer}"
    except Exception as exc:
        logger.warning(
            "system_prompt_offload: fallito (%s), system_text inviato intero", exc
        )
        return system_text


# ── Smart upscale (ADR 0016 Fase C) ─────────────────────────────────────────
# Quando il context stimato supera 0.9*window del modello attivo, cerca nella
# catalog un modello con context_window >= est_tokens*overhead_ratio
# nel tier configurato (`agent.upscale.target_tier`, default 'heavy').
# Lo switch e' visibile in UI e tracciato in `agent_runs.upscale_*`.
_UPSCALE_CACHE: dict[str, Any] = {
    "loaded_at": 0.0,
    "enabled": None,
    "overhead": None,
    "target_tier": None,
    "cost_cap": None,
}
_UPSCALE_TTL_SEC = 60.0


def _load_upscale_config() -> dict[str, Any]:
    """Carica i settings agent.upscale.* dal DB (cache 60s)."""
    now = time.time()
    if _UPSCALE_CACHE["enabled"] is not None and (
        now - _UPSCALE_CACHE["loaded_at"]
    ) < _UPSCALE_TTL_SEC:
        return {
            "enabled": _UPSCALE_CACHE["enabled"],
            "overhead": _UPSCALE_CACHE["overhead"],
            "target_tier": _UPSCALE_CACHE["target_tier"],
            "cost_cap": _UPSCALE_CACHE["cost_cap"],
        }
    enabled = True
    overhead = 1.2
    # Regola G: niente nomi modello in codice. Il tier di escalation e' nel DB
    # (agent.upscale.target_tier, default 'heavy'); _smart_upscale_model
    # interroga ai_price_catalog scegliendo il modello con il context_window
    # piu' grande tra quelli enabled+abbastanza capienti per la richiesta.
    target_tier = "heavy"
    cost_cap = 0.50
    try:
        from brain.utils.settings_db import get_bool_setting, get_setting
        enabled = get_bool_setting("agent.upscale.enabled", True)
        overhead = float(get_setting("agent.upscale.target_overhead_ratio", "1.2") or "1.2")
        target_tier = (get_setting("agent.upscale.target_tier", "heavy") or "heavy").strip()
        cost_cap = float(get_setting("agent.upscale.cost_cap_usd_per_run", "0.50") or "0.50")
    except Exception as exc:
        logger.warning("upscale: load DB fallito, uso default (%s)", exc)
    _UPSCALE_CACHE["loaded_at"] = now
    _UPSCALE_CACHE["enabled"] = enabled
    _UPSCALE_CACHE["overhead"] = overhead
    _UPSCALE_CACHE["target_tier"] = target_tier
    _UPSCALE_CACHE["cost_cap"] = cost_cap
    return {"enabled": enabled, "overhead": overhead, "target_tier": target_tier, "cost_cap": cost_cap}


_PROVIDER_FROM_MODEL_CACHE: dict[str, str] = {}


def _provider_from_model(model: str) -> str | None:
    """Risolve `provider` dal nome modello via `ai_price_catalog` (cache process)."""
    if not model:
        return None
    cached = _PROVIDER_FROM_MODEL_CACHE.get(model)
    if cached is not None:
        return cached
    try:
        import os as _os
        import psycopg2  # type: ignore[import-untyped]
        db_url = get_db_url()  # regola G: niente fallback hardcoded
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "SELECT provider FROM ai_price_catalog WHERE model = %s AND is_enabled = TRUE LIMIT 1",
                    (model,),
                )
                row = cur.fetchone()
                if row and row[0]:
                    _PROVIDER_FROM_MODEL_CACHE[model] = row[0]
                    return row[0]
    except Exception as exc:
        logger.debug("_provider_from_model: lookup fallito %s (%s)", model, exc)
    return None


def _smart_upscale_model(
    current_model: str,
    current_window: int,
    est_tokens: int,
) -> tuple[str, str] | None:
    """Ritorna `(new_model, reason)` se trova un modello con window adeguato.

    Regola G: la scelta e' tier-based (settings.agent.upscale.target_tier, default
    'heavy'), niente nomi modello in codice ne' in settings DB. Sceglie dal
    catalog il modello con context_window piu' grande nel tier configurato che
    sia abilitato e capable per tool use (run agentici).
    """
    cfg = _load_upscale_config()
    if not cfg["enabled"] or est_tokens <= 0 or current_window <= 0:
        return None
    if est_tokens < current_window * 0.9:
        return None  # il modello attuale ce la fa

    required = int(est_tokens * cfg["overhead"])
    target_tier = cfg["target_tier"]
    try:
        import psycopg2  # type: ignore[import-untyped]
        db_url = get_db_url()  # regola G: niente fallback hardcoded
        with psycopg2.connect(db_url) as conn:
            with conn.cursor() as cur:
                # Scelta DINAMICA dal catalog: tier + capability + context window.
                # Niente whitelist hardcoded: il miglior modello disponibile vince.
                cur.execute(
                    """
                    SELECT model, context_window
                    FROM ai_price_catalog
                    WHERE performance_tier = %s
                      AND is_enabled = TRUE
                      AND supports_tool_use = TRUE
                      AND agentic_thinking_policy <> 'exclude'
                      AND context_window >= %s
                    ORDER BY context_window DESC, is_featured DESC,
                             input_cost_per_million_tokens ASC NULLS LAST
                    LIMIT 1
                    """,
                    (target_tier, required),
                )
                row = cur.fetchone()
                if not row:
                    logger.warning(
                        "upscale: nessun modello tier=%s con window >= %d",
                        target_tier, required,
                    )
                    return None
                new_model = row[0]
                new_window = row[1]
                if new_model == current_model:
                    return None
                logger.info(
                    "upscale: %s (window=%d) -> %s (window=%d, tier=%s) per est_tokens=%d",
                    current_model, current_window, new_model, new_window, target_tier, est_tokens,
                )
                return (new_model, f"context_overflow:est={est_tokens}:from_window={current_window}:tier={target_tier}")
    except Exception as exc:
        logger.warning("upscale: lookup DB fallito (%s), no switch", exc)
        return None


def _estimate_context_tokens(messages: list) -> int:
    """Stima token TOTALI del context, usando tiktoken (ADR 0016 Fase D).

    Sostituisce `_estimate_context_chars(...) // 4` (che sottostima 3-5x su
    JSON/CJK/base64). Stima accurata ±2% vs payload reale al provider.
    """
    total = 0
    for m in messages:
        if hasattr(m, "content"):
            content = m.content
            if isinstance(content, str):
                total += _count_tokens(content)
            elif isinstance(content, list):
                # content puo' essere una lista di blocchi (es. tool_use/result)
                for block in content:
                    if isinstance(block, dict):
                        for v in block.values():
                            if isinstance(v, str):
                                total += _count_tokens(v)
        kwargs = getattr(m, "additional_kwargs", {}) or {}
        for block in kwargs.get("anthropic_content", []):
            if isinstance(block, dict):
                c = block.get("content", "")
                if isinstance(c, str):
                    total += _count_tokens(c)
    return total


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


def _first_human_index(messages: list[Any]) -> int:
    """Indice del PRIMO HumanMessage (la richiesta originale), -1 se assente."""
    for i, m in enumerate(messages):
        if getattr(m, "type", None) == "human":
            return i
    return -1


def _is_summary_message(m: Any) -> bool:
    """True se il messaggio e' un riassunto rolling (da preservare)."""
    extra = getattr(m, "additional_kwargs", {}) or {}
    if extra.get("nexus_summary") or extra.get("rolling_summary"):
        return True
    content = getattr(m, "content", "")
    return isinstance(content, str) and content.lstrip().startswith("[RIASSUNTO")


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
    # Freno TOKEN-based intra-turno (mig 0280). Se la stima token del contesto
    # supera max_context_ratio * context_window del modello attivo, scatta una
    # compressione AGGRESSIVA che tocca anche i messaggi assistant lunghi.
    "max_context_ratio": 0.70,
    "aggressive_keep_recent": 3,
    "aggressive_max_chars": 200,
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
        "max_context_ratio": float(_CTX_MGMT_DEFAULTS["max_context_ratio"]),
        "aggressive_keep_recent": int(_CTX_MGMT_DEFAULTS["aggressive_keep_recent"]),
        "aggressive_max_chars": int(_CTX_MGMT_DEFAULTS["aggressive_max_chars"]),
    }
    try:
        import os as _os
        import psycopg2  # type: ignore[import-untyped]
        db_url = get_db_url()  # regola G: niente fallback hardcoded
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
                elif key == "agent.context.max_context_ratio":
                    r = float(sval)
                    if 0.4 <= r <= 0.9:
                        config["max_context_ratio"] = r
                elif key == "agent.context.aggressive_keep_recent":
                    config["aggressive_keep_recent"] = max(1, int(sval))
                elif key == "agent.context.aggressive_max_chars":
                    config["aggressive_max_chars"] = max(50, int(sval))
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
        db_url = get_db_url()  # regola G: niente fallback hardcoded
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


# Sentinella a convenzione chiusa NOSTRA (formato fisso, parser legittimo):
# prefisso del tool_result quando il predictive context cap blocca una chiamata.
# Usata anche dal tool_dispatch per RIFIUTARE (una volta) un task_complete
# outcome=blocked dichiarato nello stesso turno di un blocco-cap: il blocco e'
# della SINGOLA chiamata, non del task (incidente run 5df5cef2: "quante tabelle
# nel db" chiuso blocked "per mancanza di spazio" dopo un'estrazione figma
# deragliata e bloccata dal cap, coi dati per rispondere gia' raccolti).
PREDICTIVE_CAP_SENTINEL = "[ERROR: chiamata bloccata da predictive context cap]"


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
    # Anti-deragliamento (incidente run 5df5cef2): vedi PREDICTIVE_CAP_SENTINEL.
    return (
        PREDICTIVE_CAP_SENTINEL + "\n"
        "ATTENZIONE: e' stata bloccata SOLO questa chiamata, NON il task. "
        "Se questo tool non e' essenziale per la RICHIESTA CORRENTE dell'utente "
        "(es. l'hai chiamato per via di contenuti storici della conversazione), "
        "IGNORALO e prosegui col task usando i dati che hai gia' raccolto. "
        "NON dichiarare il task bloccato per questo motivo.\n"
        f"Dettaglio: context a {current} token ({pct}% del budget {window}); il "
        f"risultato atteso aggiungerebbe ~{expected_tokens} token oltre il "
        f"{int(ratio*100)}% (cap={cap_tokens}).\n"
        "Solo se il tool e' DAVVERO necessario alla richiesta corrente:\n"
        "- Riduci i parametri (es. length piu' piccolo).\n"
        "- Usa estrazione strutturata (nexus_extract_figma_structure, "
        "nexus_extract_pdf_text, nexus_extract_docx_text).\n"
        "- Oppure dichiara con task_complete outcome=needs_input cosa serve dall'utente."
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
        dburl = _os.environ.get("DATABASE_URL")
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
