"""Regressione self-loop G1 -> nodo passthrough g1_continue.

Causa radice (diagnosi 2026-06-15): in automatic, un run che chiudeva con una
risposta di TRANSIZIONE (no tool call, intento non compiuto) veniva re-instradato
da route_after_executor sul SELF-LOOP executor->executor. Il checkpointer
PostgreSQL custom non materializza il task schedulato su un self-loop (aput_writes
NO-OP, aget_tuple senza pending_writes): il Pregel loop esauriva con un task
pendente, il grafo non raggiungeva mai il learner e si chiudeva 'completed' su un
task NON convergente.

Fix (Opzione A): nodo passthrough g1_continue (executor -> g1_continue ->
executor) che forza un confine di superstep distinto, come tool_dispatch nei run
convergenti, piu' chiusura onesta al cap (forced_close_unverified=True).

Questi test sono DETERMINISTICI (nessun LLM, nessun DB Postgres): usano il VERO
route_after_executor e il VERO g1_continue_node su un mini-grafo con MemorySaver,
e invocano direttamente il ramo g1_cap di executor_node con dipendenze mockate.

Esecuzione senza pytest:
    python3 -c "import brain.tests.test_g1_continue_topology as t; t.run_all()"
"""
from __future__ import annotations

import asyncio

from langchain_core.messages import AIMessage, HumanMessage
from langgraph.checkpoint.memory import MemorySaver
from langgraph.graph import END, StateGraph

import brain.agents.nodes as nodes
from brain.agents.nodes import g1_continue_node
from brain.agents.nodes.routing import route_after_executor
from brain.agents.state import AgentState


def _build_minigraph(executor_fn):
    """Mini-grafo che riproduce la topologia post-fix:

        executor --route_after_executor--> {g1_continue|learner}
        g1_continue --> executor
        learner --> END

    Riusa il VERO route_after_executor e il VERO g1_continue_node: e' la
    topologia che conta, non l'implementazione dell'executor (stub). Compilato
    con MemorySaver, un checkpointer CONFORME (a differenza di quello custom):
    serve a provare che, ELIMINATO il self-loop, il grafo avanza al secondo
    superstep executor. Con il self-loop il superstep G1 non avveniva.
    """

    async def _learner(state: AgentState) -> dict:
        return {"result": "closed"}

    g = StateGraph(AgentState)
    g.add_node("executor", executor_fn)
    g.add_node("g1_continue", g1_continue_node)
    g.add_node("learner", _learner)
    g.set_entry_point("executor")
    g.add_conditional_edges(
        "executor",
        route_after_executor,
        {
            "tool_dispatch": "learner",  # non usato in questo test
            "verifier": "learner",       # non usato in questo test
            "final_gate": "learner",     # non usato in questo test
            "learner": "learner",
            "g1_continue": "g1_continue",
        },
    )
    g.add_edge("g1_continue", "executor")
    g.add_edge("learner", END)
    return g.compile(checkpointer=MemorySaver())


def test_g1_reroute_avanza_al_secondo_superstep_executor():
    """Pre-fix: il self-loop non avanzava -> il grafo non raggiungeva learner.
    Post-fix: executor -> g1_continue -> executor (secondo giro), poi chiude.

    Lo stub dell'executor al 1o giro ritorna end_turn + stato che fa scattare
    route_after_executor -> "g1_continue" (intento non compiuto, automatic, no
    tool call, action_oriented). Al 2o giro segnala done -> learner -> END.
    """
    calls = {"n": 0}

    async def _executor_stub(state: AgentState) -> dict:
        calls["n"] += 1
        if calls["n"] == 1:
            # Chiusura descrittiva: end_turn, nessun pending, nessuna azione
            # produttiva in history -> route G1 deve re-instradare a g1_continue.
            return {
                "stop_reason": "end_turn",
                "pending_tool_uses": [],
                "iterations": 1,
                "g1_reroute_count": 0,
            }
        # Secondo giro: dichiara done cosi' il route va a learner (chiusura).
        return {
            "stop_reason": "end_turn",
            "pending_tool_uses": [],
            "iterations": 2,
            "declared_outcome": {"outcome": "done", "summary": "fatto"},
        }

    graph = _build_minigraph(_executor_stub)

    initial_state = {
        "messages": [HumanMessage(content="crea il componente X e collegalo")],
        "automation_mode": "automatic",
        "action_oriented": True,
        "tools_json": [{"name": "edit_file"}],
        "iterations": 0,
        "g1_reroute_count": 0,
        "pending_tool_uses": [],
        "stop_reason": None,
        "plan_phase_active": False,
    }
    config = {"configurable": {"thread_id": "t-g1-topo"}, "recursion_limit": 25}

    async def _run() -> dict:
        return await graph.ainvoke(initial_state, config=config)

    final_state = asyncio.run(_run())

    # L'executor DEVE essere stato eseguito due volte: prova che il superstep G1
    # e' avanzato (pre-fix, col self-loop non materializzato, si fermava a 1).
    assert calls["n"] >= 2, (
        "executor non ha eseguito il secondo superstep: il re-routing G1 non e' "
        "avanzato (regressione self-loop)"
    )
    # Il grafo ha raggiunto learner e chiuso (no task pendente orfano).
    assert final_state.get("result") == "closed"


def test_route_after_executor_g1_ritorna_g1_continue_non_executor():
    """Guard di topologia: il re-routing G1 NON deve mai ritornare 'executor'
    (self-loop), ma 'g1_continue'. Verifica diretta della route, senza grafo."""
    state = {
        "messages": [HumanMessage(content="implementa la feature Y e testala")],
        "automation_mode": "automatic",
        "action_oriented": True,
        "tools_json": [{"name": "edit_file"}],
        "iterations": 1,
        "g1_reroute_count": 0,
        "pending_tool_uses": [],
        "stop_reason": "end_turn",
        "plan_phase_active": False,
    }
    assert route_after_executor(state) == "g1_continue"


def test_g1_escalated_ritorna_g1_continue():
    """Anche il ramo g1_escalated (re-execution post escalation) passa per
    g1_continue: era un altro self-loop executor->executor."""
    state = {
        "messages": [HumanMessage(content="vai")],
        "stop_reason": "g1_escalated",
        "iterations": 2,
        "pending_tool_uses": [],
    }
    assert route_after_executor(state) == "g1_continue"


def test_g1_cap_imposta_forced_close_unverified(monkeypatch):
    """Al G1 cap (reroute esaurito + escalation esaurita) il dict ritornato da
    executor_node deve avere forced_close_unverified=True, cosi' mcp-core mappa
    FailedDiagnosed (chiusura onesta) invece del ramo else -> Completed."""
    # Neutralizza le dipendenze esterne (DB/cancellazione/escalation) cosi' il
    # flusso raggiunge il ramo cap in modo deterministico.
    monkeypatch.setattr(nodes, "_check_superseded", lambda state: False)
    monkeypatch.setattr(nodes, "_load_g1_max_nudges", lambda: 3)
    # Catena di escalation ESAURITA: nessun modello piu' capace -> ramo cap.
    monkeypatch.setattr(
        nodes, "_pick_escalation_model", lambda prov, model, esc: None
    )
    # orchestrator_config.get() e' usato dal DAG scheduler gate: forniamo un dict
    # senza DAG/plan cosi' lo scheduler non parte e si arriva al ramo G1.
    monkeypatch.setattr(
        nodes.orchestrator_config,
        "get",
        lambda: {"dag_parallel_enabled": False},
    )

    state = {
        "messages": [
            HumanMessage(content="crea il modulo Z"),
            AIMessage(content="Ora procedo. Per prima cosa ispeziono il package.json..."),
        ],
        "automation_mode": "automatic",
        "action_oriented": True,
        "tools_json": [{"name": "edit_file"}],
        "iterations": 5,
        # reroute gia' al cap + escalation gia' esaurita -> ramo cap.
        "g1_reroute_count": 3,
        "auto_escalations": 3,
        "pending_tool_uses": [],
        "stop_reason": "end_turn",
        "plan_phase_active": False,
        "thread_id": "t-g1-cap",
    }

    async def _run() -> dict:
        return await nodes.executor_node(state)

    result = asyncio.run(_run())

    assert result.get("stop_reason") == "g1_cap_reached"
    assert result.get("forced_close_unverified") is True, (
        "il ramo g1_cap deve marcare forced_close_unverified=True (chiusura onesta "
        "-> FailedDiagnosed), altrimenti mcp-core chiude Completed un run non convergente"
    )


def run_all():
    import inspect

    fns = [
        (k, v)
        for k, v in sorted(globals().items())
        if k.startswith("test_") and callable(v)
    ]
    for name, fn in fns:
        sig = inspect.signature(fn)
        if "monkeypatch" in sig.parameters:
            # Eseguibile senza pytest: salta i test che richiedono la fixture.
            print("  skip (richiede monkeypatch):", name)
            continue
        fn()
    print("test_g1_continue_topology: OK (%d test)" % len(fns))


if __name__ == "__main__":
    run_all()
