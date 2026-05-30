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
_providers = None
_routing_client = None


def configure(tool_runner: Any, providers: Any = None, routing_client: Any = None) -> None:
    """Inject del ToolRunnerClient gRPC + (Cluster 3) provider registry e
    routing client per la verifica esplorativa LLM economica."""
    global _tool_runner, _providers, _routing_client
    _tool_runner = tool_runner
    _providers = providers
    _routing_client = routing_client


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

    # ── Cluster 3: verifica esplorativa RAG-informed (additiva, gated) ───────
    # Solo se i criteri deterministici sono passati: un passo LLM economico
    # cerca anomalie NON coperte dai criterion pre-definiti, informato dai
    # pattern di fallimento passati (RAG). Il deterministico resta primario:
    # al cap si promuove comunque.
    if all_passed and bool(cfg.get("exploratory_verify_enabled")):
        expl_cap = int(cfg.get("exploratory_verify_max_cycles", 1) or 1)
        expl_cycle = int(state.get("exploratory_verify_cycle", 0) or 0)
        if expl_cycle < expl_cap:
            expl_ok, expl_finding = await _run_exploratory_check(state, todo, results, ctx, cfg)
            if not expl_ok and expl_finding:
                logger.info("verifier_node: verifica esplorativa ha trovato un'anomalia non coperta")
                hint = (
                    f"<verifica_esplorativa cycle=\"{expl_cycle + 1}/{expl_cap}\">\n"
                    f"I criteri deterministici passano, ma un controllo aggiuntivo ha "
                    f"rilevato un possibile problema non coperto:\n{expl_finding}\n"
                    f"Valuta se correggerlo prima di considerare il todo completato.\n"
                    f"</verifica_esplorativa>"
                )
                return {
                    "messages": [HumanMessage(content=hint)],
                    "verify_cycle": cycle,
                    "exploratory_verify_cycle": expl_cycle + 1,
                    "stop_reason": "tool_use",
                    "pending_tool_uses": [],
                }
            # passato o niente finding: prosegui come completato.

    # ── Branch su esito ───────────────────────────────────────────────────
    if all_passed:
        _mark_todo_status(active_todo_id, "completed")
        advance = _advance_or_end(run_id)
        advance["verify_cycle"] = 0
        advance["exploratory_verify_cycle"] = 0
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

    # Retry: appendi messaggio <verification_failed> e torna a executor.
    # Fix audit 27/05/2026: in modalita' automatico/continuo, prepend un blocco
    # <autonomy_hint> per forzare l'agente a procedere senza chiedere conferma.
    # Senza questo, il modello (specie gemini-2.0-flash-lite) interpretava il
    # blocco verification_failed come "chiedi all'utente" e ripeteva "Vuoi che
    # lo faccia?" / "Confermi?" anche in autonomia.
    failed_block_text = _render_failed_block(todo, cycle, max_cycles, results)
    behavior_mode = (state.get("behavior_mode") or "").strip().lower()
    is_autonomous = behavior_mode in ("automatic", "automatico", "continuous", "continuo")
    if is_autonomous:
        autonomy_prefix = (
            "<autonomy_hint mode=\"" + behavior_mode + "\">\n"
            "L'utente ha selezionato la modalita' '" + behavior_mode + "': procedi\n"
            "AUTONOMAMENTE col retry. NON chiedere conferma all'utente, NON\n"
            "scrivere domande tipo 'Vuoi che lo faccia?' o 'Confermi?'. Esegui\n"
            "direttamente le azioni necessarie usando i tool disponibili per\n"
            "risolvere i criteri di accettazione falliti. Se non riesci dopo\n"
            "questo ciclo, l'agente verra' automaticamente bloccato dal verifier\n"
            "al raggiungimento del cap di " + str(max_cycles) + " cicli.\n"
            "</autonomy_hint>\n\n"
        )
        failed_block_text = autonomy_prefix + failed_block_text
    hm = HumanMessage(content=failed_block_text)
    return {
        "messages": [hm],
        "verify_cycle": cycle,
        "verifier_last_result": {"passed": False, "cycle": cycle, "results": results},
        "stop_reason": "tool_use",  # forza un'altra iterazione di executor
        "pending_tool_uses": [],
    }


# ─── Helpers ──────────────────────────────────────────────────────────────


async def _run_exploratory_check(
    state: AgentState, todo: dict, results: list[dict], ctx: dict, cfg: dict,
) -> tuple[bool, str]:
    """Cluster 3: verifica esplorativa LLM economica RAG-informed.

    Ritorna (ok, finding): ok=True se nessun problema; ok=False + finding se
    rileva un'anomalia non coperta dai criteri deterministici. Best-effort:
    su qualunque errore ritorna (True, "") per non bloccare (deterministico
    primario). Usa SOLO servizi Nexus esistenti: nexus_search_semantic (RAG
    fallimenti) + purpose_model('exploratory_verify') + provider registry.
    """
    if _providers is None or _routing_client is None:
        return True, ""
    todo_content = str(todo.get("content") or "").strip()
    if not todo_content:
        return True, ""

    # 1. RAG dei fallimenti passati via il tool semantico esistente (kb +
    #    chat_history catturano correzioni/problemi gia' incontrati). Niente
    #    client Qdrant nuovo (condizione di integrazione).
    past_failures = ""
    try:
        if _tool_runner is not None:
            topk = int(cfg.get("exploratory_verify_topk", 5) or 5)
            res = await _tool_runner.execute_tool(
                tool_name="nexus_search_semantic",
                tool_input={
                    "query": f"problemi o errori ricorrenti su: {todo_content}",
                    "source_kinds": ["kb", "chat_history"],
                    "top_k": topk,
                },
                session_id=str(ctx.get("session_id") or ""),
                tool_use_id=str(uuid.uuid4()),
            )
            raw = getattr(res, "result_json", None) or "{}"
            hits = (json.loads(raw).get("hits") or [])[:topk]
            past_failures = "\n".join(
                f"- {str(h.get('chunk_text') or '')[:200]}" for h in hits if h.get("chunk_text")
            )
    except Exception as exc:
        logger.debug("verifier_node: RAG fallimenti skip (%s)", exc)

    # 2. Risolvi il modello economico via purpose model (regola G).
    try:
        decision = _routing_client.purpose_model(purpose="exploratory_verify")
        provider, model = decision.provider, decision.model
        if not provider or provider.startswith("__"):
            return True, ""
    except Exception as exc:
        logger.debug("verifier_node: purpose_model(exploratory_verify) fallito (%s)", exc)
        return True, ""

    # 3. Chiamata LLM economica: ispeziona l'esito e segnala SOLO problemi
    #    concreti non gia' coperti dai criteri deterministici (gia' passati).
    crit_summary = "; ".join(str(r.get("type")) for r in results) or "(nessuno)"
    prompt = (
        "Sei un revisore di qualita'. Un task e' stato completato e i controlli "
        "automatici deterministici sono PASSATI.\n\n"
        f"Task: {todo_content}\n"
        f"Controlli gia' verificati (NON ripeterli): {crit_summary}\n"
    )
    if past_failures:
        prompt += f"\nProblemi ricorrenti su task simili (dalla memoria):\n{past_failures}\n"
    prompt += (
        "\nEsiste un problema CONCRETO non coperto dai controlli sopra "
        "(es. effetto collaterale, caso limite ignorato, incoerenza)? "
        "Rispondi in una riga: se tutto ok scrivi esattamente 'OK'. "
        "Altrimenti scrivi 'PROBLEMA: <descrizione sintetica>'."
    )
    try:
        result = await __import__("asyncio").to_thread(
            _providers.generate_completion, provider, model, prompt
        )
        text = (getattr(result, "content", "") or "").strip()
    except Exception as exc:
        logger.debug("verifier_node: LLM esplorativo fallito (%s)", exc)
        return True, ""

    if text.upper().startswith("PROBLEMA"):
        finding = text.split(":", 1)[1].strip() if ":" in text else text
        return False, finding[:500]
    return True, ""


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
