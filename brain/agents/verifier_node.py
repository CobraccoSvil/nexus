"""verifier_node (PR-2): verifica deterministica della DoD post-executor.

Si attiva quando l'executor emette `stop_reason="end_turn"` (modello pensa
di aver finito). Il verifier:
  1. Carica gli acceptance_criteria del todo attivo da `nexus_agent_todos`
  2. Esegue ogni criterion via criteria_runner (deterministic, no LLM)
  3. Persiste il run su `nexus_agent_verifier_runs`
  4a. Se tutti pass: marca todo `completed`, prende prossimo todo, ritorna a executor (next todo)
  4b. Se almeno uno fallisce + verify_cycle < max: appende HumanMessage `<verification_failed>`
      e ritorna a executor (retry su stesso todo)
  4c. Se cap raggiunto: marca todo `blocked`, prossimo se disponibile, oppure end_turn

Tutte le scritture su DB sono best-effort (graceful degrade su connection error).
"""
from __future__ import annotations

import json
import logging
import os
import time
import uuid
from typing import Any

from langchain_core.messages import HumanMessage

from . import orchestrator_config, prompt_registry, todo_store, criteria_runner
from .state import AgentState

logger = logging.getLogger(__name__)

# Servizi iniettati
_tool_runner = None


def configure(tool_runner: Any) -> None:
    """Inject del ToolRunnerClient gRPC."""
    global _tool_runner
    _tool_runner = tool_runner


async def verifier_node(state: AgentState) -> dict[str, Any]:
    """Verifica la DoD del todo attivo (PR-2)."""
    cfg = orchestrator_config.get()

    # ── Guards ────────────────────────────────────────────────────────────
    if not cfg["verifier_enabled"] or not state.get("plan_phase_active"):
        return {}

    run_id = state.get("thread_id")
    active_todo_id = state.get("active_todo_id")
    if not run_id:
        logger.debug("verifier_node: thread_id assente, skip")
        return {}

    # Se non c'e' todo attivo, prova a calcolarlo
    if not active_todo_id:
        active = todo_store.active_todo(run_id)
        if not active:
            logger.debug("verifier_node: nessun todo attivo, skip")
            return {}
        active_todo_id = active.get("id")

    # ── Carica todo + acceptance_criteria ─────────────────────────────────
    todos = todo_store.list_todos(run_id)
    todo = next((t for t in todos if t.get("id") == active_todo_id), None)
    if not todo:
        logger.warning("verifier_node: todo %s non trovato nel DB", active_todo_id)
        return {}

    criteria_raw = todo.get("acceptance_criteria") or []
    if isinstance(criteria_raw, str):
        try:
            criteria_raw = json.loads(criteria_raw)
        except Exception:
            criteria_raw = []
    if not criteria_raw:
        # Nessun criterion: marca completed e passa al prossimo
        _mark_todo_status(active_todo_id, "completed")
        return _advance_or_end(run_id)

    # ── Esegui tutti i criteria ────────────────────────────────────────────
    ctx = {
        "tool_runner": _tool_runner,
        "session_id": state.get("session_id"),
        "project_id": state.get("project_id") or os.environ.get("NEXUS_PROJECT_ID", ""),
        "timeout_s": cfg["verifier_timeout_s"],
    }
    results: list[dict[str, Any]] = []
    started = time.monotonic()
    for c in criteria_raw:
        if "id" not in c:
            c["id"] = str(uuid.uuid4())
        try:
            ok, evidence = await criteria_runner.run_criterion(c, ctx)
        except Exception as exc:
            logger.error("verifier_node: criterion %s exception: %s", c.get("type"), exc)
            ok, evidence = False, {"error": str(exc)}
        results.append({
            "id": c["id"],
            "type": c.get("type"),
            "passed": ok,
            "evidence": evidence,
        })

    duration_ms = int((time.monotonic() - started) * 1000)
    all_passed = all(r["passed"] for r in results)
    cycle = int(state.get("verify_cycle", 0) or 0) + 1

    # Persistenza best-effort
    _persist_verifier_run(run_id, active_todo_id, cycle, results, all_passed, duration_ms)

    logger.info(
        "verifier_node: todo=%s cycle=%d passed=%s (%d criteria, %dms)",
        active_todo_id, cycle, all_passed, len(results), duration_ms,
    )

    # ── Branch su esito ───────────────────────────────────────────────────
    if all_passed:
        _mark_todo_status(active_todo_id, "completed")
        advance = _advance_or_end(run_id)
        advance["verify_cycle"] = 0
        return advance

    max_cycles = int(cfg["max_verify_cycles"])
    if cycle >= max_cycles:
        # Cap raggiunto → marca blocked, prossimo se possibile
        _mark_todo_status(active_todo_id, "blocked")
        logger.warning(
            "verifier_node: todo %s blocked dopo %d cicli falliti", active_todo_id, cycle,
        )
        advance = _advance_or_end(run_id)
        advance["verify_cycle"] = 0
        advance["verifier_last_result"] = {"passed": False, "cycle": cycle, "results": results}
        return advance

    # Retry: appendi messaggio <verification_failed> e torna a executor
    failed_block_text = _render_failed_block(todo, cycle, max_cycles, results)
    hm = HumanMessage(content=failed_block_text)
    return {
        "messages": [hm],
        "verify_cycle": cycle,
        "verifier_last_result": {"passed": False, "cycle": cycle, "results": results},
        "stop_reason": "tool_use",  # forza un'altra iterazione di executor
        "pending_tool_uses": [],
    }


# ─── Helpers ──────────────────────────────────────────────────────────────


def _advance_or_end(run_id: str) -> dict[str, Any]:
    """Sceglie il prossimo todo pending e aggiorna lo state.

    Se nessun todo pending: ritorna end_turn (il loop terminera').
    """
    todos = todo_store.list_todos(run_id)
    next_pending = next((t for t in todos if t.get("status") == "pending"), None)
    if next_pending is None:
        all_done = all(t.get("status") in ("completed", "skipped") for t in todos)
        logger.info(
            "verifier_node: tutti i todo terminali (all_done=%s, total=%d)",
            all_done, len(todos),
        )
        return {"active_todo_id": None, "stop_reason": "end_turn"}
    # Marca il nuovo come in_progress
    _mark_todo_status(next_pending["id"], "in_progress")
    return {
        "active_todo_id": next_pending["id"],
        "stop_reason": "tool_use",
        "current_todos": todos,
    }


def _mark_todo_status(todo_id: str, new_status: str) -> None:
    """UPDATE diretto sullo status del todo (best-effort)."""
    if not todo_id:
        return
    try:
        import psycopg2  # type: ignore[import-untyped]
        url = os.environ.get("DATABASE_URL", "")
        if not url:
            return
        conn = psycopg2.connect(url)
        try:
            with conn.cursor() as cur:
                cur.execute(
                    """UPDATE nexus_agent_todos
                       SET status = %s, updated_at = NOW(),
                           verify_failures = CASE WHEN %s = 'blocked'
                                                  THEN verify_failures + 1
                                                  ELSE verify_failures END
                       WHERE id = %s""",
                    (new_status, new_status, todo_id),
                )
            conn.commit()
        finally:
            conn.close()
    except Exception as exc:
        logger.warning("verifier_node._mark_todo_status %s -> %s fallito: %s", todo_id, new_status, exc)


def _persist_verifier_run(
    run_id: str, todo_id: str, cycle: int, results: list[dict], passed: bool, duration_ms: int,
) -> None:
    try:
        import psycopg2  # type: ignore[import-untyped]
        from psycopg2.extras import Json  # type: ignore[import-untyped]
        url = os.environ.get("DATABASE_URL", "")
        if not url:
            return
        conn = psycopg2.connect(url)
        try:
            with conn.cursor() as cur:
                cur.execute(
                    """INSERT INTO nexus_agent_verifier_runs
                       (run_id, todo_id, cycle, criteria_results, passed, duration_ms)
                       VALUES (%s, %s, %s, %s, %s, %s)""",
                    (run_id, todo_id, cycle, Json(results), passed, duration_ms),
                )
            conn.commit()
        finally:
            conn.close()
    except Exception as exc:
        logger.warning("verifier_node._persist_verifier_run fallita: %s", exc)


def _render_failed_block(
    todo: dict, cycle: int, max_cycles: int, results: list[dict],
) -> str:
    """Rendering del HumanMessage <verification_failed> da iniettare al retry.

    Usa il template `verification.failed_block` (mig 0149) se presente,
    altrimenti fallback inline.
    """
    failed = [r for r in results if not r["passed"]]
    failed_rendered = "\n".join(
        f"- [{r.get('type')}] {json.dumps(r.get('evidence', {}), ensure_ascii=False)[:300]}"
        for r in failed
    )
    diagnostic = ""
    if failed and failed[0].get("evidence"):
        ev = failed[0]["evidence"]
        diagnostic = ev.get("output_excerpt") or ev.get("error") or ""
    remediation = _suggest_remediation(failed)

    tpl = prompt_registry.get_prompt("verification.failed_block") or ""
    if tpl:
        return (
            tpl.replace("{{cycle}}", str(cycle))
            .replace("{{max_cycles}}", str(max_cycles))
            .replace("{{todo_content}}", todo.get("content", ""))
            .replace("{{failed_criteria_rendered}}", failed_rendered)
            .replace("{{diagnostic_output}}", diagnostic[:800])
            .replace("{{remediation_hint}}", remediation)
        )
    return (
        f"<verification_failed cycle=\"{cycle}/{max_cycles}\" todo=\"{todo.get('content','')}\">\n"
        f"Acceptance criteria falliti:\n{failed_rendered}\n\n"
        f"Output diagnostico:\n{diagnostic[:800]}\n\n"
        f"Suggerimento operativo: {remediation}\n"
        f"</verification_failed>"
    )


def _suggest_remediation(failed: list[dict]) -> str:
    """Heuristic semplice per generare un hint di rimedio basato sul tipo di
    criterion fallito. Niente LLM call: regola stringhe."""
    if not failed:
        return "verifica i criteri e riprova"
    first = failed[0]
    ev = first.get("evidence", {}) or {}
    t = first.get("type")
    if t == "http":
        status = ev.get("status")
        if status is None:
            return "il servizio HTTP non risponde: verifica che sia avviato sulla porta corretta"
        if int(status or 0) >= 500:
            return f"HTTP {status}: errore lato server, leggi i log del servizio per la causa"
        if int(status or 0) == 404:
            return f"HTTP 404: la route non esiste, registra l'endpoint nel router"
        return f"HTTP {status} != atteso, verifica la risposta del servizio"
    if t == "run_command":
        exit_c = ev.get("exit_code")
        return f"comando ritorna exit_code={exit_c}: leggi STDERR e correggi"
    if t == "file_exists":
        return "il file non esiste sul filesystem: scrivilo con write_file"
    if t == "db_query":
        notes = ev.get("notes") or []
        return ("; ".join(notes) if notes else "verifica lo schema e lo stato del DB")
    if t == "regex_in_output":
        return "il pattern atteso non e' presente nell'output: rivedi il comando o l'output"
    return "rivedi il criterion e applica una correzione mirata"
