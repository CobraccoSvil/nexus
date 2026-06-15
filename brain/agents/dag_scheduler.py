"""dag_scheduler (Comp.3b): esecuzione parallela dei layer del DAG.

Forma strutturata e MUTUAMENTE ESCLUSIVA del worker-mode (PR-C): quando il
piano ha dipendenze (depends_on) e dag_parallel_enabled e' ON, invece di
eseguire un todo alla volta lo scheduler calcola il "ready layer" (tutti i todo
pending le cui dipendenze sono gia' completate) e li delega IN PARALLELO ai
sub-agent via il tool MCP `dispatch_subagents` (Comp.0), a ondate con un cap
conservativo.

Vincoli di sicurezza (rispettati dal design):
  - Parallelismo confinato in UNA chiamata tool (dispatch_subagents fa join_all
    su /agent/subagent-run): lo state LangGraph resta un dict serializzabile,
    niente fan-out nativo che esploderebbe i checkpoint.
  - Cap = min(dag_max_parallel, max_parallel_subagents). Cost gate ereditato da
    dispatch_subagent (cost_cap_per_run).
  - cascade_skip: se un todo di un layer fallisce, i todo che dipendono da esso
    (diretta o transitivamente) vengono marcati 'skipped' per non sprecare run.

Best-effort: qualunque errore -> ritorna senza marcare progressi, il loop
storico/sequenziale riprende il controllo. Gated da dag_parallel_enabled.
"""
from __future__ import annotations

import json
import logging
import uuid
from typing import Any

from . import todo_store

logger = logging.getLogger(__name__)

# Kind di sub-agent generico per l'esecuzione di un todo. Deve essere in
# orchestrator.subagent_kinds_whitelist; 'implement' e' un kind pre-definito.
_DEFAULT_TODO_KIND = "implement"


def compute_ready_layer(todos: list[dict]) -> list[dict]:
    """Ritorna i todo pending le cui dipendenze sono tutte completed/skipped.

    E' il fronte eseguibile in parallelo del DAG. Se nessun todo ha dipendenze,
    ritorna tutti i pending (il chiamante applichera' il cap).
    """
    done = {str(t.get("id")) for t in todos if t.get("status") in ("completed", "skipped")}
    ready: list[dict] = []
    for t in todos:
        if t.get("status") != "pending":
            continue
        deps = t.get("depends_on") or []
        if all(str(d) in done for d in deps):
            ready.append(t)
    return ready


def should_parallelize(ready: list[dict], todos: list[dict], cfg: dict) -> bool:
    """Decide se attivare il DAG parallelo (Ultra, decomposizione parallela).

    True se esiste un ready layer e:
      - ci sono dipendenze esplicite fra i todo (comportamento storico), OPPURE
      - ci sono almeno `dag_parallel_min_ready` todo ready: i todo INDIPENDENTI
        (nessun depends_on) sono il caso piu' parallelizzabile, prima bloccato
        dalla sola guardia _has_deps (col vecchio planner i todo non avevano mai
        depends_on -> il DAG parallelo non scattava quasi mai).

    Con `dag_parallel_min_ready` <= 1 resta il comportamento storico (parallelo
    solo quando ci sono dipendenze esplicite). Punto unico della decisione
    (regola L): l'executor delega qui invece di re-implementare la guardia.
    """
    if not ready:
        return False
    has_deps = any(t.get("depends_on") for t in todos)
    min_ready = int(cfg.get("dag_parallel_min_ready", 2) or 2)
    return has_deps or (min_ready >= 2 and len(ready) >= min_ready)


def _descendants(todo_id: str, todos: list[dict]) -> set[str]:
    """Insieme dei todo che dipendono (diretta/transitivamente) da todo_id."""
    children: dict[str, list[str]] = {}
    for t in todos:
        for d in t.get("depends_on") or []:
            children.setdefault(str(d), []).append(str(t.get("id")))
    out: set[str] = set()
    stack = [todo_id]
    while stack:
        cur = stack.pop()
        for c in children.get(cur, []):
            if c not in out:
                out.add(c)
                stack.append(c)
    return out


def _mark(todo_id: str, status: str) -> None:
    """UPDATE best-effort dello status (riusa lo stesso meccanismo del verifier)."""
    import os
    if not todo_id:
        return
    try:
        url = os.environ.get("DATABASE_URL")
        if not url:
            return
        from brain.utils.db_pool import connect as _db_connect
        with _db_connect() as conn, conn.cursor() as cur:
            cur.execute(
                "UPDATE nexus_agent_todos SET status = %s, updated_at = NOW() WHERE id = %s",
                (status, todo_id),
            )
    except Exception as exc:
        logger.warning("dag_scheduler._mark %s -> %s fallito: %s", todo_id, status, exc)


async def run_dag_layer(
    state: dict, tool_runner: Any, cfg: dict
) -> dict[str, Any]:
    """Esegue UNA ondata di todo ready in parallelo via dispatch_subagents.

    Ritorna updates per lo state: lista degli id eseguiti (active_todo_ids) e
    stop_reason. Se non c'e' nulla da eseguire ritorna {} (il chiamante decide
    se terminare o passare al sequenziale). Best-effort.
    """
    run_id = state.get("thread_id")
    if not run_id or tool_runner is None:
        return {}
    todos = todo_store.list_todos(run_id)
    ready = compute_ready_layer(todos)
    if not ready:
        return {}

    cap = min(
        int(cfg.get("dag_max_parallel", 2) or 2),
        int(cfg.get("max_parallel_subagents", 3) or 3),
    )
    cap = max(1, cap)
    layer = ready[:cap]

    # Marca i todo del layer come in_progress prima di delegare.
    for t in layer:
        _mark(str(t.get("id")), "in_progress")

    tasks = [
        {
            "kind": _DEFAULT_TODO_KIND,
            "task": str(t.get("content") or ""),
            "expected_output_format": "riepilogo conciso delle modifiche applicate",
        }
        for t in layer
    ]

    logger.info(
        "dag_scheduler: ondata parallela di %d todo (cap=%d, run_id=%s)",
        len(layer), cap, run_id,
    )
    try:
        res = await tool_runner.execute_tool(
            tool_name="dispatch_subagents",
            tool_input={"tasks": tasks, "max_parallel": cap},
            session_id=str(state.get("session_id") or ""),
            tool_use_id=str(uuid.uuid4()),
        )
        raw = getattr(res, "result_json", None) or "{}"
        data = json.loads(raw)
    except Exception as exc:
        logger.warning("dag_scheduler: dispatch_subagents fallito (%s)", exc)
        # Ripristina i todo a pending: il sequenziale riprovera'.
        for t in layer:
            _mark(str(t.get("id")), "pending")
        return {}

    results = data.get("results") or []
    executed_ids: list[str] = []
    completed = 0
    # Allinea per posizione: dispatch_subagents preserva l'ordine dei task.
    # Il parallelismo e' confinato in questa singola ondata (lo scheduler viene
    # invocato in loop dall'executor finche' ci sono ready layer): la
    # promozione del todo si basa sull'esito del sub-agent, perche' il verifier
    # come nodo separato non gira tra le ondate di questo path.
    for t, r in zip(layer, results):
        tid = str(t.get("id"))
        executed_ids.append(tid)
        if isinstance(r, dict) and r.get("error") is None:
            _mark(tid, "completed")
            completed += 1
        else:
            # Fallimento: marca blocked + cascade_skip dei discendenti.
            _mark(tid, "blocked")
            for desc in _descendants(tid, todos):
                _mark(desc, "skipped")
            logger.warning("dag_scheduler: todo %s fallito, cascade_skip discendenti", tid)

    return {
        "active_todo_ids": executed_ids,
        "completed": completed,
        "stop_reason": "tool_use",
    }
