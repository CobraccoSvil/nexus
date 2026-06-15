"""todo_runner_node — esecuzione SEQUENZIALE dei todo come sub-run ISOLATI.

Strategia Claude Code (opzione 3): i todo del planner NON girano nel loop
executor principale (che accumula history turno dopo turno, facendo degradare
il modello e impedendo la chiusura end-to-end). Vengono eseguiti uno alla volta
come SUB-RUN ISOLATE con context fresco, sul modello di come Claude Code spawna
sub-agent.

Mattoni RIUSATI (regola L, punto unico — niente duplicazione):
  - run_subagent (via il tool MCP `dispatch_subagents`, lo STESSO path usato da
    dag_scheduler per i todo PARALLELI): crea una sub-run isolata con state
    fresco, thread_id figlio, no history del main. Qui e' la versione
    SEQUENZIALE: max_parallel=1, un todo per volta in ordine di seq.
  - todo_store / verifier_node._mark_todo_status / _pick_next_todo: stesso
    todo store e stessa semantica di avanzamento.
  - dag_scheduler._descendants: cascade-skip dei discendenti su fallimento.
  - session_worklog: lo STATO CONDIVISO provider-agnostico tra i sub-run (puro
    testo nel system_text del sub-grafo, gia' cablato dal router_node).

Reversibilita': il nodo e' raggiunto SOLO se orchestrator_config.todo_isolation_active
e' True (gate a 3 condizioni, setting DEFAULT FALSE). Difesa in profondita': se
viene raggiunto comunque (es. setting tolto a meta' run), il guard interno
ritorna {} e il routing di uscita instrada all'executor classico. Con setting
OFF il comportamento del grafo e' identico a prima (edge planner->executor).
"""
from __future__ import annotations

import json
import logging
import uuid
from typing import Any

from langchain_core.messages import HumanMessage

from . import orchestrator_config, todo_store
from .dag_scheduler import _descendants
from .state import AgentState
from .verifier_node import _mark_todo_status, _pick_next_todo

logger = logging.getLogger(__name__)

# Limite del summary compatto del sub-run che confluisce nel context del
# prossimo todo (coerente con subagent_dispatch_node._compact_summary).
_SUMMARY_MAX_CHARS = 600

# Servizio iniettato (ToolRunnerClient gRPC), come per verifier_node.
_tool_runner = None


def configure(tool_runner: Any) -> None:
    """Inject del ToolRunnerClient gRPC (chiamato da graph.create_agent_graph)."""
    global _tool_runner
    _tool_runner = tool_runner


def _compact(text: str, max_chars: int = _SUMMARY_MAX_CHARS) -> str:
    """Tronca un summary: il main accumula solo questo, non l'intera sub-conv."""
    text = str(text or "").strip()
    if len(text) <= max_chars:
        return text
    suffix = "...[troncato]"
    return text[: max_chars - len(suffix)] + suffix


def _build_context_blob(
    state: AgentState, todo: dict, prior_results: list[dict]
) -> str:
    """Costruisce il context del sub-run (testo provider-neutro).

    Componenti (regola: niente accumulo di history grezza, solo stato compatto):
      a. worklog di sessione corrente — gia' iniettato dal router_node del
         sub-grafo via session_id ereditato; lo passiamo comunque ESPLICITAMENTE
         qui per non dipendere dalla freschezza del digest materializzato.
      b. summary compatti dei todo gia' completati in questo run
         (state["subagent_results"]).
      c. rationale + constraints del piano (prodotti dal planner_node).
      d. acceptance_criteria del todo come "Definition of Done".
    """
    parts: list[str] = []

    # (a) worklog di sessione (best-effort, fail-open).
    try:
        from . import session_worklog as _sw
        wl = _sw.fetch_worklog_block(str(state.get("session_id") or ""))
        if wl:
            parts.append(wl)
    except Exception as exc:  # pragma: no cover - difensivo
        logger.debug("todo_runner: worklog skip (%s)", exc)

    # (b) esiti compatti dei todo gia' completati in questo run.
    if prior_results:
        done_lines = []
        for r in prior_results[-8:]:
            seq = r.get("seq")
            content = str(r.get("content") or "")[:120]
            status = r.get("status") or "?"
            summary = _compact(str(r.get("summary") or ""), 240)
            done_lines.append(f"  - todo {seq} ({status}): {content} -> {summary}")
        parts.append(
            "<todo_gia_eseguiti>\n"
            "I seguenti passi del piano sono gia' stati eseguiti in sub-run "
            "isolate (non rifarli, costruisci sopra il loro esito):\n"
            + "\n".join(done_lines)
            + "\n</todo_gia_eseguiti>"
        )

    # (c) rationale + constraints del piano.
    rationale = str(state.get("plan_rationale") or "").strip()
    constraints = state.get("plan_constraints") or []
    if rationale or constraints:
        block = ["<piano>"]
        if rationale:
            block.append(f"  <rationale>{rationale[:1200]}</rationale>")
        if constraints:
            items = "\n".join(f"    - {str(c)[:200]}" for c in constraints[:10])
            block.append(f"  <vincoli>\n{items}\n  </vincoli>")
        block.append("</piano>")
        parts.append("\n".join(block))

    # (d) acceptance_criteria del todo come Definition of Done.
    criteria = todo.get("acceptance_criteria") or []
    if isinstance(criteria, str):
        try:
            criteria = json.loads(criteria)
        except Exception:
            criteria = []
    if criteria:
        crit_lines = []
        for c in criteria[:10]:
            if isinstance(c, dict):
                ctype = c.get("type") or "criterio"
                expected = c.get("expected") or c.get("description") or ""
                crit_lines.append(f"    - [{ctype}] {str(expected)[:200]}")
        if crit_lines:
            parts.append(
                "<definition_of_done>\n"
                "Il passo e' completo SOLO se questi criteri sono soddisfatti:\n"
                + "\n".join(crit_lines)
                + "\n</definition_of_done>"
            )

    return "\n\n".join(parts)


def _todo_kind(cfg: dict) -> str:
    """Kind del sub-agent per l'esecuzione di un todo (DB-driven, regola G).

    Allineato a dag_scheduler._DEFAULT_TODO_KIND; deve essere in
    orchestrator.subagent_kinds_whitelist.
    """
    kind = str(cfg.get("todo_isolation_kind") or "").strip()
    return kind or "implement"


async def _dispatch_one(
    state: AgentState, todo: dict, cfg: dict, extra_context: str = ""
) -> dict[str, Any] | None:
    """Esegue UN todo come sub-run isolata via il tool MCP dispatch_subagents
    (max_parallel=1). Ritorna il dict result del sub-run, o None se il dispatch
    stesso e' fallito (whitelist/errore tool), nel qual caso il chiamante fa
    fallback all'executor classico.
    """
    prior = list(state.get("subagent_results") or [])
    context_blob = _build_context_blob(state, todo, prior)
    if extra_context:
        context_blob = (extra_context + "\n\n" + context_blob).strip()

    task = str(todo.get("content") or "").strip()
    tasks = [
        {
            "kind": _todo_kind(cfg),
            "task": task,
            "context": context_blob,
            "expected_output_format": (
                "riepilogo conciso delle modifiche applicate e dell'esito"
            ),
        }
    ]
    try:
        res = await _tool_runner.execute_tool(
            tool_name="dispatch_subagents",
            tool_input={"tasks": tasks, "max_parallel": 1},
            session_id=str(state.get("session_id") or ""),
            tool_use_id=str(uuid.uuid4()),
        )
        raw = getattr(res, "result_json", None) or "{}"
        data = json.loads(raw)
    except Exception as exc:
        logger.warning("todo_runner: dispatch_subagents fallito (%s)", exc)
        return None

    results = data.get("results") or []
    if not results:
        logger.warning("todo_runner: dispatch_subagents ha ritornato results vuoto")
        return None
    return results[0] if isinstance(results[0], dict) else None


def _result_failed(result: dict) -> bool:
    """True se il sub-run NON e' andato a buon fine.

    Il tool ritorna `error` solo per fallimenti di dispatch (whitelist/insert);
    `status` distingue completed/failed/timeout della sub-run vera e propria.
    """
    if result.get("error") is not None:
        return True
    status = str(result.get("status") or "completed").strip().lower()
    return status not in ("completed", "completed_verified")


async def todo_runner_node(state: AgentState) -> dict[str, Any]:
    """Esegue il prossimo todo pending come sub-run isolata (re-entry per todo).

    Ritorna un dict-patch dello state (mai mutazione in-place). stop_reason
    pilota il routing di uscita (route_after_todo_runner):
      - "tool_use": c'e' ancora lavoro -> re-entry su todo_runner.
      - "end_turn": catena finita o bloccata -> final_gate/learner.
      - assente: dispatch fallito -> fallback all'executor classico.
    """
    cfg = orchestrator_config.get()

    # (1) Guard difensivo: il routing non sarebbe dovuto arrivare qui se
    # l'isolamento non e' attivo. Ritorna {} -> route_after_todo_runner
    # instrada all'executor classico (fallback al comportamento storico).
    if not orchestrator_config.todo_isolation_active(state):
        logger.info("todo_runner_node: isolamento non attivo, no-op (fallback executor)")
        return {}

    if _tool_runner is None:
        logger.warning("todo_runner_node: tool_runner non iniettato, fallback executor")
        return {}

    run_id = state.get("thread_id")
    if not run_id:
        logger.debug("todo_runner_node: thread_id assente, fallback")
        return {}

    todos = todo_store.list_todos(run_id)
    if not todos:
        logger.info("todo_runner_node: nessun todo nel piano, chiudo")
        return {"active_todo_id": None, "stop_reason": "end_turn"}

    # (3) Prossimo todo pending in ordine di seq (riusa la semantica DAG-aware
    # del verifier; sequenziale puro = primo pending).
    next_todo = _pick_next_todo(todos, cfg)
    if next_todo is None:
        all_done = all(t.get("status") in ("completed", "skipped") for t in todos)
        logger.info(
            "todo_runner_node: tutti i todo terminali (all_done=%s, total=%d) -> end_turn",
            all_done, len(todos),
        )
        return {"active_todo_id": None, "stop_reason": "end_turn"}

    todo_id = str(next_todo.get("id"))
    seq = next_todo.get("seq")
    content = str(next_todo.get("content") or "")

    # (4) Marca in_progress prima di delegare.
    _mark_todo_status(todo_id, "in_progress")

    logger.info(
        "todo_runner_node: dispatch sub-run isolata todo seq=%s id=%s run_id=%s",
        seq, todo_id, run_id,
    )

    # (6) Delega via run_subagent (path dispatch_subagents, max_parallel=1).
    result = await _dispatch_one(state, next_todo, cfg)

    # Errore di DISPATCH (non del sub-run): ripristina pending e fallback
    # all'executor classico, cosi' il loop storico riprende il controllo.
    if result is None:
        _mark_todo_status(todo_id, "pending")
        logger.warning(
            "todo_runner_node: dispatch fallito per todo %s, fallback executor", todo_id,
        )
        return {}

    # (7-8) Esito del sub-run -> promozione/blocco + gestione fallimenti.
    accumulated = list(state.get("subagent_results") or [])
    summary = _compact(str(result.get("summary") or ""))
    cost = float(result.get("cost_usd") or 0.0)
    tokens = result.get("tokens") or {}
    record = {
        "seq": seq,
        "todo_id": todo_id,
        "content": content[:200],
        "summary": summary,
        "cost_usd": cost,
    }

    on_failure = str(cfg.get("todo_isolation_on_failure") or "stop").strip().lower()

    if not _result_failed(result):
        _mark_todo_status(todo_id, "completed")
        record["status"] = "completed"
        accumulated.append(record)
        _append_worklog_fact(state, seq, content, "completed", summary)
        patch = _advance_patch(run_id, cfg, accumulated, cost, tokens)
        logger.info("todo_runner_node: todo %s completato -> prossimo", todo_id)
        return patch

    # ── Fallimento del sub-run ────────────────────────────────────────────
    record["status"] = "failed"
    accumulated.append(record)
    _append_worklog_fact(state, seq, content, "failed", summary)

    if on_failure == "retry":
        retries_done = int(state.get("todo_isolation_retries") or 0)
        max_retries = int(cfg.get("todo_isolation_max_retries") or 1)
        if retries_done < max_retries:
            logger.warning(
                "todo_runner_node: todo %s fallito, retry %d/%d con context arricchito",
                todo_id, retries_done + 1, max_retries,
            )
            _mark_todo_status(todo_id, "pending")
            err_ctx = (
                "<tentativo_precedente_fallito>\n"
                "Il passo e' gia' stato tentato e NON e' riuscito. Esito del "
                "tentativo precedente:\n" + summary + "\n"
                "Affronta la causa del fallimento prima di riprovare.\n"
                "</tentativo_precedente_fallito>"
            )
            retry_result = await _dispatch_one(state, next_todo, cfg, extra_context=err_ctx)
            if retry_result is not None and not _result_failed(retry_result):
                _mark_todo_status(todo_id, "completed")
                record["status"] = "completed_after_retry"
                record["summary"] = _compact(str(retry_result.get("summary") or ""))
                _append_worklog_fact(
                    state, seq, content, "completed", record["summary"]
                )
                return _advance_patch(
                    run_id, cfg, accumulated,
                    cost + float((retry_result or {}).get("cost_usd") or 0.0),
                    (retry_result or {}).get("tokens") or {},
                    extra_retries=1,
                )
            # Retry fallito -> degrada a "stop".
            logger.warning("todo_runner_node: retry di %s fallito, degrado a stop", todo_id)

    if on_failure == "continue":
        # Best-effort: blocca questo, prosegui col prossimo pending NON dipendente.
        _mark_todo_status(todo_id, "blocked")
        for desc in _descendants(todo_id, todos):
            _mark_todo_status(desc, "skipped")
        logger.warning(
            "todo_runner_node: todo %s blocked (on_failure=continue), prosegui", todo_id,
        )
        return _advance_patch(run_id, cfg, accumulated, cost, tokens)

    # on_failure == "stop" (DEFAULT) o degrado dal retry: blocca + cascade-skip
    # dei discendenti + chiusura onesta verso final_gate/learner.
    _mark_todo_status(todo_id, "blocked")
    for desc in _descendants(todo_id, todos):
        _mark_todo_status(desc, "skipped")
    logger.warning(
        "todo_runner_node: todo %s blocked (on_failure=stop) -> chiusura catena", todo_id,
    )
    return {
        "active_todo_id": todo_id,
        "stop_reason": "end_turn",
        "subagent_results": accumulated,
        "subagent_cost_cumulative_usd": float(
            state.get("subagent_cost_cumulative_usd") or 0.0
        ) + cost,
    }


def _advance_patch(
    run_id: str,
    cfg: dict,
    accumulated: list[dict],
    cost: float,
    tokens: dict,
    *,
    extra_retries: int = 0,
) -> dict[str, Any]:
    """Costruisce il patch di avanzamento: marca il prossimo todo in_progress
    (se esiste) e ritorna stop_reason di re-entry o chiusura.
    """
    todos = todo_store.list_todos(run_id)
    nxt = _pick_next_todo(todos, cfg)
    base: dict[str, Any] = {
        "subagent_results": accumulated,
        "subagent_cost_cumulative_usd": cost,
    }
    if extra_retries:
        base["todo_isolation_retries"] = extra_retries
    if nxt is None:
        base["active_todo_id"] = None
        base["stop_reason"] = "end_turn"
        return base
    base["active_todo_id"] = str(nxt.get("id"))
    base["stop_reason"] = "tool_use"
    base["current_todos"] = todos
    return base


def _append_worklog_fact(
    state: AgentState, seq: Any, content: str, status: str, summary: str
) -> None:
    """Confluenza dell'avanzamento todo nello stato condiviso.

    I tool-call dei sub-run (file/comandi/errori) finiscono gia' nel worklog di
    sessione perche' girano sotto lo STESSO session_id (collect_step_facts lato
    mcp-core). Qui aggiungiamo SOLO un messaggio sintetico per-todo nello state
    del main, cosi' la catena resta esplicita anche se il digest materializzato
    e' in ritardo. Niente tabella nuova, niente writer nuovo.
    """
    # Best-effort, mai bloccante: il fatto vive nel patch messages del nodo
    # (ritornato dal chiamante via accumulated/record). Qui logghiamo per
    # osservabilita'; la confluenza testuale primaria e' il context_blob del
    # prossimo sub-run.
    logger.info(
        "todo_runner: todo %s '%s' -> %s: %s",
        seq, content[:80], status, _compact(summary, 200),
    )
