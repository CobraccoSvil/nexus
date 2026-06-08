"""Funzioni di routing condizionale del grafo LangGraph di Nexus.

Le route_after_* e route_by_task_type decidono il prossimo nodo in base allo
state. Estratte da nodes.py mantenendo nomi e logica identici. Importano gli
helper puri da .helpers e i moduli fratelli (orchestrator_config, final_gate)
via import locale dentro le funzioni dove gia' avveniva, per evitare cicli.
"""
from __future__ import annotations

import logging
from typing import Any

from .. import orchestrator_config
from ..state import AgentState
from .helpers import (
    MAX_AGENT_ITERATIONS,
    _detect_action_request,
    _detect_unfulfilled_intent,
    _load_g1_max_nudges,
    _load_tool_choice_forcing_config,
    structural_unfulfilled_signal,
)

logger = logging.getLogger(__name__)


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
    # Cancellazione cooperativa (single-run-per-session): executor_node ha
    # rilevato che questo run e' stato superato/cancellato sulla stessa sessione.
    # Chiusura immediata: il loop in memoria termina, niente altri step in chat.
    if stop_reason == "superseded":
        logger.warning("route_after_executor: run superato (last-wins), chiusura cooperativa")
        return "learner"
    # G1 escalation: l'executor ha promosso il turno a un modello piu' capace
    # (sticky_model aggiornato, reroute azzerato). Ri-entra nell'executor per
    # far agire il modello escalato. Il cap iterazioni globale resta la safety.
    if stop_reason == "g1_escalated":
        logger.warning("route_after_executor: G1 escalation orchestratore -> re-executor")
        return "executor"
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
            # Re-routing G1: scatta sia quando la richiesta originale e'
            # action-oriented, sia quando l'ULTIMA risposta del modello ha
            # annunciato un'azione imminente senza eseguirla (intenzione non
            # compiuta). Il secondo caso copre i debug dove il primo messaggio
            # umano non e' imperativo ma il modello narra "Inizio verificando X"
            # e chiude. Cap g1_reroute_count previene loop su falsi positivi.
            _is_action_req = _detect_action_request(str(_first_human))
            _is_unfulfilled = _detect_unfulfilled_intent(state.get("result"))
            # Gating modalita': in confirm l'utente vuole controllo step-by-step,
            # quindi una mera intenzione/attesa narrata e non eseguita NON innesca
            # auto-azione (re-entry); l'executor produce un resoconto onesto.
            # action_req e structural restano attivi in ogni modalita'.
            _automation_mode = (state.get("automation_mode") or "confirm").strip().lower()
            _unfulfilled_triggers = _is_unfulfilled and _automation_mode in (
                "automatic",
                "continuous",
            )
            # ── ADR 0018 (c): segnale STRUTTURALE primario ───────────────────
            # Il caso BookingPage (0 tool call su un task d'azione mentre i tool
            # erano disponibili) scatta per via strutturale, indipendentemente
            # dai verbi del testo. had_tools_available = tools_json non vuoto;
            # no_tool_call_this_turn = nessun pending (gia' garantito dal gate
            # `not pending` del ramo); action_oriented = richiesta utente
            # d'azione. La soglia iterazione riusa il config del tool_choice
            # forcing (stessa nozione di "primi turni d'azione").
            _had_tools = bool(state.get("tools_json"))
            _tc_enabled, _tc_max_iter = _load_tool_choice_forcing_config()
            _structural_unfulfilled = structural_unfulfilled_signal(
                had_tools_available=_had_tools,
                no_tool_call_this_turn=not pending,
                action_oriented=_is_action_req,
                iteration=iterations,
                max_iteration=_tc_max_iter,
            )
            if _structural_unfulfilled or _is_action_req or _unfulfilled_triggers:
                _nudge_count_log = int(state.get("action_nudge_count") or 0)
                if _structural_unfulfilled:
                    _trigger = "structural(had_tools+no_tool_call+action)"
                elif _is_action_req:
                    _trigger = "textual(action-request)"
                else:
                    _trigger = "textual(intent-non-compiuta)"
                logger.warning(
                    "route_after_executor: G1 risposta descrittiva, segnale=%s "
                    "(iter=%d reroute=%d/%d nudge=%d) -> re-executor",
                    _trigger,
                    iterations, _reroute_count, _max_nudges, _nudge_count_log,
                )
                return "executor"
        else:
            logger.warning(
                "route_after_executor: G1 cap reroute raggiunto "
                "(iter=%d reroute=%d/%d) -> chiusura forzata via learner",
                iterations, _reroute_count, _max_nudges,
            )
    # Final gate generale (fail-closed): per task software che chiudono SENZA
    # plan_phase (il verifier non gira), un gate minimo verifica che il codice
    # importato non resti orfano (app placeholder). Indipendente dal planner.
    if not state.get("plan_phase_active"):
        try:
            # orchestrator_config e' gia' importato a livello modulo (riga ~27).
            # NON re-importarlo qui: un import locale lo renderebbe variabile
            # locale per TUTTA la funzione, e l'uso precedente a riga ~4004
            # (ramo plan_phase_active) solleverebbe UnboundLocalError quando un
            # piano e' attivo. Importiamo solo final_gate (non globale, cicli).
            from .. import final_gate as _fg
            _cfg = orchestrator_config.get()
            if _cfg.get("final_gate_enabled") and _fg._is_software_task(state, _cfg):
                _fc = int(state.get("final_gate_cycle", 0) or 0)
                if _fc < int(_cfg["final_gate_max_cycles"]):
                    logger.info("route_after_executor: software task end_turn senza plan -> final_gate")
                    return "final_gate"
        except Exception as _e:
            logger.debug("route_after_executor: final_gate skip (%s)", _e)
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


def route_after_regression_gate(state: AgentState) -> str:
    """Routing post-regression_gate (M13.4/M13.5).

    SOFT (default): il nodo ritorna {} -> stop_reason invariato -> 'learner'.
    HARD block (M13.5, default-OFF): il nodo ha settato stop_reason='tool_use'
    e incrementato regression_cycle -> rientra in 'executor' per il fix.
    """
    if state.get("stop_reason") == "tool_use":
        return "executor"
    return "learner"


def route_by_task_type(state: AgentState) -> str:
    """Routing condizionale: mappa task_type al nodo executor."""
    # Tutti i task_type validi vanno verso executor
    return "executor"
