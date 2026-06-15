"""final_gate: gate generale fail-closed per task software senza plan_phase.

Il verifier_node gira solo quando il piano e' attivo (`plan_phase_active`). Per i
task software che chiudono SENZA plan (executor diretto, end_turn) non c'era
alcuna verifica: un'app placeholder (hello-world) montata sopra un design
importato passava silenziosamente (fail-open).

Questo modulo chiude quel buco riusando il motore di verifica generale
(`criteria_runner`), in particolare il criterio `no_orphan_imported`: se esiste
codice staged in figma_export/ con abbastanza moduli, l'entry servito
(src/main.tsx) deve raggiungerli via grafo degli import. Hello-world -> fallisce
(re-executor); design montato -> passa (chiude); nessuno staging -> N/A.

Tutta la configurazione e' letta da `orchestrator_config` (settings DB, cache
60s). Nessun nome modello / valore hardcoded (regola G).
"""
from __future__ import annotations

import logging
import os
from typing import Any

from langchain_core.messages import HumanMessage

from . import orchestrator_config, criteria_runner

logger = logging.getLogger(__name__)

# ToolRunnerClient gRPC iniettato dal graph builder (come verifier_node).
_tool_runner = None


def configure(tool_runner: Any) -> None:
    """Inject del ToolRunnerClient usato dai criteri generali."""
    global _tool_runner
    _tool_runner = tool_runner


def _is_software_task(state: dict[str, Any], cfg: dict[str, Any]) -> bool:
    """True se il run va trattato come task software (quindi verificabile dal gate).

    Due segnali in OR (de-lessicalizzazione, incidente Beauty-Book 2026-06-11):
    1. STRUTTURALE (primario): il run ha gia' eseguito tool che MUTANO il
       filesystem/progetto (write/edit/rename/estrazioni/comandi). Un run che ha
       toccato il progetto va verificato A PRESCINDERE dall'intent classificato:
       il caso reale era intent=architecture (fuori whitelist) che aveva spostato
       file con rename e ha chiuso senza alcuna verifica.
    2. Whitelist intent (legacy, `agent.final_gate.software_intents`): copre i
       run software che chiudono senza step mutativi (es. solo pianificazione
       che DEVE comunque passare dal gate per il no-orphan check).

    Lo state usa `user_intent` (popolato da router_node); fallback su `intent`.
    """
    try:
        from .nodes.helpers import has_filesystem_mutation_in_history
        if has_filesystem_mutation_in_history(state.get("messages") or []):
            return True
    except Exception as exc:  # pragma: no cover - difensivo
        logger.debug("final_gate: check strutturale mutazioni saltato (%s)", exc)
    intent = str(state.get("user_intent") or state.get("intent") or "").lower()
    if not intent:
        return False
    software_intents = [str(i).lower() for i in (cfg.get("final_gate_software_intents") or [])]
    return intent in software_intents


def _resolve_build_command(state: dict[str, Any]) -> tuple[str, str | None] | None:
    """Risolve il comando build del progetto per il criterio di COMPILAZIONE del
    final_gate (fix qualita' 2026-06-15). Priorita':
      1. run_configurations del progetto con label ~ 'build' o role='build'
         (fonte per-progetto canonica);
      2. setting `agent.final_gate.build_command` (auto-detect generico default).
    Ritorna (command, working_dir|None), oppure None se il build-check e'
    disabilitato o nessun comando e' risolvibile. Best-effort: su errore DB
    ritorna None (N/A, non blocca la chiusura)."""
    try:
        from brain.utils.db_pool import connect as _db_connect
    except Exception:
        return None
    project_id = state.get("project_id") or os.environ.get("NEXUS_PROJECT_ID", "")
    try:
        with _db_connect() as conn, conn.cursor() as cur:
            cur.execute(
                "SELECT value FROM settings WHERE key = 'agent.final_gate.build_check_enabled'"
            )
            row = cur.fetchone()
            enabled = (row[0] if row and row[0] else "true").strip().lower() in ("true", "1", "yes")
            if not enabled:
                return None
            if project_id:
                cur.execute(
                    "SELECT command, args, cwd FROM run_configurations "
                    "WHERE project_id = %s "
                    "AND (lower(label) LIKE %s OR lower(coalesce(role, '')) = 'build') "
                    "ORDER BY (lower(coalesce(role, '')) = 'build') DESC LIMIT 1",
                    (project_id, "%build%"),
                )
                rc = cur.fetchone()
                if rc and rc[0]:
                    command, args, cwd = rc[0], rc[1] or [], rc[2]
                    full = command + ((" " + " ".join(args)) if args else "")
                    return (full, cwd)
            cur.execute(
                "SELECT value FROM settings WHERE key = 'agent.final_gate.build_command'"
            )
            row = cur.fetchone()
            cmd = (row[0] if row and row[0] else "").strip()
            if cmd:
                return (cmd, None)
    except Exception as exc:  # pragma: no cover - difensivo
        logger.debug("final_gate._resolve_build_command: %s", exc)
    return None


def _build_timeout_s() -> float:
    """Timeout (s) del criterio build, da settings (default 180: i build sono
    lenti, i 30s del verifier non basterebbero)."""
    try:
        from brain.utils.db_pool import connect as _db_connect
        with _db_connect() as conn, conn.cursor() as cur:
            cur.execute(
                "SELECT value FROM settings WHERE key = 'agent.final_gate.build_timeout_s'"
            )
            row = cur.fetchone()
            return float(row[0]) if row and row[0] else 180.0
    except Exception:
        return 180.0


async def run_general_gates(
    state: dict[str, Any], cfg: dict[str, Any]
) -> tuple[bool, list[dict[str, Any]]]:
    """Esegue i criteri generali via criteria_runner.

    Per ora un unico criterio: `no_orphan_imported` (anti-placeholder).
    Best-effort: su eccezione di un criterio, passed=False con evidence error.

    Ritorna (all_passed, results) con results = lista di
    {type, passed, evidence}.
    """
    project_id = state.get("project_id") or os.environ.get("NEXUS_PROJECT_ID", "")
    ctx = {
        "tool_runner": _tool_runner,
        "session_id": state.get("session_id"),
        "project_id": project_id,
        "timeout_s": cfg.get("verifier_timeout_s", 30),
    }

    criteria: list[dict[str, Any]] = [
        {
            "type": "no_orphan_imported",
            "spec": {
                "staging_dir": cfg.get("import_staging_dirs") or ["figma_export"],
                "min_reached_ratio": cfg.get("no_orphan_min_ratio", 0.4),
            },
            "expected": {"mounted": True},
        },
        # Claim-vs-fatti (incidente Beauty-Book 2026-06-11): gli output dichiarati
        # dagli STEP del run (write/edit/rename-to) devono esistere su disco a
        # fine run. Strutturale puro (agent_steps -> filesystem), nessuna lettura
        # del final_answer. N/A se il run non ha step mutativi file.
        {
            "type": "outputs_exist",
            "spec": {"run_id": str(state.get("thread_id") or "")},
            "expected": {},
        },
    ]
    # Verifica runtime E2E (mig 0374): i log dei servizi non devono contenere
    # errori runtime. Cattura il pattern "codice scritto ma flusso reale rotto"
    # (es. endpoint 500 perche' una tabella manca) che l'agente ignorerebbe.
    if cfg.get("final_gate_runtime_check_enabled"):
        criteria.append({
            "type": "service_logs_clean",
            "spec": {
                "command": cfg.get("final_gate_runtime_log_command")
                or "docker compose logs --tail 200 --no-color 2>&1 | tail -n 200",
                "patterns": cfg.get("final_gate_runtime_error_patterns") or [],
            },
            "expected": {},
        })

    # Criterio BUILD (fix qualita' 2026-06-15): il codice deve COMPILARE prima
    # di chiudere "completed", non solo esistere (outputs_exist). Comando
    # risolto per-progetto (run_config 'build' -> setting auto-detect); N/A se
    # non risolvibile -> non blocca i progetti senza build. Timeout dedicato
    # (i build sono lenti).
    build = _resolve_build_command(state)
    if build is not None:
        build_cmd, build_cwd = build
        build_crit: dict[str, Any] = {
            "type": "run_command",
            "spec": {"command": build_cmd},
            "expected": {"exit_code": 0},
            "timeout_s": _build_timeout_s(),
        }
        if build_cwd:
            build_crit["spec"]["working_dir"] = build_cwd
        criteria.append(build_crit)

    results: list[dict[str, Any]] = []
    for c in criteria:
        try:
            ok, evidence = await criteria_runner.run_criterion(c, ctx)
        except Exception as exc:
            logger.error("final_gate: criterion %s exception: %s", c.get("type"), exc)
            ok, evidence = False, {"error": str(exc)}
        results.append({
            "type": c.get("type"),
            "passed": bool(ok),
            "evidence": evidence,
        })

    all_passed = all(r["passed"] for r in results)
    return all_passed, results


def _render_failed_block(
    state: dict[str, Any], cycle: int, max_cycles: int, results: list[dict[str, Any]]
) -> str:
    """Costruisce il testo del HumanMessage da iniettare quando il gate fallisce.

    Rispetta la modalita' autonoma (automatic/continuo) prependendo un blocco
    <autonomy_hint> come fa verifier_node._render_failed_block.
    """
    # Corpo specifico per criterio fallito: ogni criterio (no_orphan_imported,
    # service_logs_clean, ...) fornisce gia' il suo output_excerpt con diagnosi +
    # "AGISCI". Li aggreghiamo invece di un testo fisso (prima parlava solo del
    # caso Figma; ora copre anche gli errori runtime).
    failed = [r for r in results if not r.get("passed")]
    body_parts: list[str] = []
    for r in failed:
        ev = r.get("evidence") or {}
        excerpt = ev.get("output_excerpt") or ev.get("verdict") or ev.get("error") or ""
        if excerpt:
            body_parts.append(f"[{r.get('type')}]\n{str(excerpt)[:900]}")
    detail = "\n\n".join(body_parts) if body_parts else "Una verifica del gate e' fallita."

    body = (
        f"<final_gate_failed cycle=\"{cycle}/{max_cycles}\">\n"
        "Verifica pre-chiusura FALLITA. NON dichiarare il task completato finche'\n"
        "non e' risolto e RIVERIFICATO esercitando il flusso reale.\n\n"
        f"{detail}\n"
        "</final_gate_failed>"
    )

    behavior_mode = (state.get("behavior_mode") or "").strip().lower()
    is_autonomous = behavior_mode in ("automatic", "automatico", "continuous", "continuo")
    if is_autonomous:
        autonomy_prefix = (
            "<autonomy_hint mode=\"" + behavior_mode + "\">\n"
            "L'utente ha selezionato la modalita' '" + behavior_mode + "': procedi\n"
            "AUTONOMAMENTE con l'integrazione. NON chiedere conferma, NON scrivere\n"
            "domande tipo 'Vuoi che lo faccia?' o 'Confermi?'. Esegui direttamente\n"
            "le modifiche necessarie usando i tool disponibili.\n"
            "</autonomy_hint>\n\n"
        )
        body = autonomy_prefix + body
    return body


async def final_gate_node(state: dict[str, Any]) -> dict[str, Any]:
    """Gate generale fail-closed.

    - Pass-through ({}): se disabilitato o task non software.
    - Passa: chiude (stop_reason end_turn -> reflection).
    - Cap raggiunto: chiude comunque (niente loop infinito).
    - Fallisce: inietta verdetto e rimanda all'executor (stop_reason tool_use).
    """
    cfg = orchestrator_config.get()
    if not cfg.get("final_gate_enabled") or not _is_software_task(state, cfg):
        return {}

    cycle = int(state.get("final_gate_cycle", 0) or 0) + 1
    max_cycles = int(cfg["final_gate_max_cycles"])

    passed, results = await run_general_gates(state, cfg)

    if passed:
        logger.info("final_gate: passato (cycle=%d) -> chiusura", cycle)
        # Segnale per la macchina a stati di terminazione (mig 0386): la verifica
        # E2E e' passata -> esito canonico CompletedVerified lato mcp-core. NON
        # impostato sul ramo forced_close (abort: resta FailedDiagnosed).
        return {
            "final_gate_cycle": 0,
            "stop_reason": "end_turn",
            "final_gate_passed": True,
        }

    # Chiusura SENZA re-executor quando:
    #  - forced_close_unverified: siamo qui per un ABORT anti-loop. L'agente e'
    #    gia' dichiarato bloccato; rimandarlo all'executor lo fa ri-abortire,
    #    accumulando un secondo AIMessage identico -> messaggio finale DUPLICATO
    #    (bug osservato) e un mini-loop abort<->final_gate. La verifica E2E e'
    #    stata comunque eseguita una volta sopra: chiudiamo.
    #  - cap raggiunto: chiusura per evitare loop infinito.
    forced_close = bool(state.get("forced_close_unverified"))
    if forced_close or cycle >= max_cycles:
        logger.warning(
            "final_gate: chiusura senza re-executor (forced_close=%s, cycle=%d/%d)",
            forced_close, cycle, max_cycles,
        )
        return {"final_gate_cycle": 0, "stop_reason": "end_turn"}

    logger.info("final_gate: fallito (cycle=%d/%d) -> re-executor", cycle, max_cycles)
    hm = HumanMessage(content=_render_failed_block(state, cycle, max_cycles, results))
    return {
        "messages": [hm],
        "final_gate_cycle": cycle,
        "stop_reason": "tool_use",
        "pending_tool_uses": [],
    }


def route_after_final_gate(state: dict[str, Any]) -> str:
    """Routing post-final_gate: re-executor su tool_use, altrimenti chiusura."""
    return "executor" if state.get("stop_reason") == "tool_use" else "learner"
