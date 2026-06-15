"""Definizione e compilazione del grafo LangGraph per Nexus."""
from __future__ import annotations

import logging
from typing import Any

from langgraph.graph import END, StateGraph  # type: ignore[import-untyped]

from .nodes import (
    configure_services,
    executor_node,
    learner_node,
    reflection_node,
    route_after_executor,
    route_after_verifier,
    route_after_planner,
    route_after_todo_runner,
    router_node,
    tool_dispatch_node,
)
from .planner_node import planner_node, configure as _configure_planner
from .verifier_node import verifier_node, configure as _configure_verifier
from .todo_runner_node import todo_runner_node, configure as _configure_todo_runner
from .final_gate import (
    final_gate_node,
    route_after_final_gate,
    configure as _configure_final_gate,
)
from .understanding_node import understanding_node, configure as _configure_understanding
from .clarify_or_expand_node import (
    clarify_or_expand_node,
    configure as _configure_clarify,
)
from .state import AgentState

logger = logging.getLogger(__name__)


def route_after_router(state: AgentState) -> str:
    """PR-1: after router, decide se passare dal planner_node o direttamente
    all'executor (legacy path).

    Il planner_node ha il suo guard interno (orchestrator_config.is_eligible)
    e si auto-skippa se non eligibile, ma l'edge condizionale evita anche
    di chiamare la funzione quando il routing intent ha gia' fatto fallback
    a chat o quando il behavior_mode esclude il flusso. Il vantaggio principale
    e' che lo state non viene mutato (plan_phase_active resta unset) cosi'
    il loop legacy ha overhead zero.
    """
    from . import orchestrator_config
    behavior_mode = state.get("behavior_mode")
    intent = state.get("user_intent")
    token_budget = int(state.get("token_budget") or 0)
    cfg = orchestrator_config.get()
    # PR-D: gating adattivo. is_eligible_adaptive integra i segnali del
    # classifier (complexity/confidence/agentic/ambiguity) quando
    # adaptive_gating_enabled e' ON; altrimenti si comporta come is_eligible.
    eligible = orchestrator_config.is_eligible_adaptive(
        behavior_mode,
        intent,
        token_budget,
        complexity=state.get("task_complexity"),
        confidence=state.get("intent_confidence"),
        agentic_score=state.get("agentic_score"),
        is_ambiguous=state.get("is_ambiguous"),
    )
    logger.info(
        "route_after_router: eligible=%s plan_enabled=%s adaptive=%s mode=%r intent=%r "
        "budget=%d complexity=%r agentic=%r -> %s",
        eligible, cfg.get("plan_phase_enabled"), cfg.get("adaptive_gating_enabled"),
        behavior_mode, intent, token_budget,
        state.get("task_complexity"), state.get("agentic_score"),
        "planner" if eligible else "executor",
    )
    return "planner" if eligible else "executor"


def create_agent_graph(
    providers: Any,
    router: Any,
    embeddings: Any,
    tool_runner: Any = None,
    agent_router: Any = None,
    agentic_classifier: Any = None,
) -> Any:
    """Crea e compila il grafo LangGraph con tutti i servizi Nexus.

    Args:
        providers: ProviderRegistry globale di Nexus
        router: SemanticRouter globale di Nexus
        embeddings: EmbeddingService globale di Nexus
        tool_runner: Runner opzionale per tool dispatch.
        agent_router: Router agente opzionale.

    Returns:
        Grafo compilato con PostgresCheckpointer e interrupt_before=["executor"].
    """
    from brain.memory.retrieval import InteractionRetriever
    from brain.memory.storage import PostgresLearningStorage

    from .checkpointer import create_checkpointer

    # Inizializza storage PostgreSQL (sostituisce il vecchio SQLite locale)
    storage = PostgresLearningStorage()
    retriever = InteractionRetriever(embedding_service=embeddings)

    # Inietta servizi nei nodi
    configure_services(
        providers=providers,
        router=router,
        embeddings=embeddings,
        storage=storage,
        retriever=retriever,
        tool_runner=tool_runner,
        agent_router=agent_router,
        agentic_classifier=agentic_classifier,
    )

    # PR-1: inject anche nel planner_node. routing_client deriva dal router
    # se ha la stessa interfaccia (purpose_model), altrimenti il planner si
    # skippa al primo run.
    try:
        from brain.router.service import _routing_client_singleton
        _routing_client = _routing_client_singleton()
    except Exception as exc:
        logger.warning("create_agent_graph: routing_client non disponibile (planner sara' inattivo): %s", exc)
        _routing_client = None
    _configure_planner(providers=providers, tool_runner=tool_runner, routing_client=_routing_client)
    _configure_verifier(tool_runner=tool_runner, providers=providers, routing_client=_routing_client)
    # Final gate generale (fail-closed): riusa il tool_runner per i criteri
    # generali (no_orphan_imported) sui task software senza plan_phase.
    _configure_final_gate(tool_runner=tool_runner)
    _configure_clarify(providers=providers, routing_client=_routing_client, tool_runner=tool_runner)
    _configure_understanding(providers=providers, tool_runner=tool_runner, routing_client=_routing_client)
    # Esecuzione sequenziale isolata dei todo (mig 0431): riusa il tool_runner
    # per delegare ogni todo a run_subagent via dispatch_subagents.
    _configure_todo_runner(tool_runner=tool_runner)

    # Crea il grafo con lo schema di stato
    workflow: StateGraph = StateGraph(AgentState)

    # Aggiunge nodi
    workflow.add_node("router", router_node)  # type: ignore[arg-type]
    workflow.add_node("clarify_or_expand", clarify_or_expand_node)  # type: ignore[arg-type]
    workflow.add_node("understanding", understanding_node)  # type: ignore[arg-type]
    workflow.add_node("planner", planner_node)  # type: ignore[arg-type]
    workflow.add_node("todo_runner", todo_runner_node)  # type: ignore[arg-type]
    workflow.add_node("executor", executor_node)  # type: ignore[arg-type]
    workflow.add_node("tool_dispatch", tool_dispatch_node)  # type: ignore[arg-type]
    workflow.add_node("verifier", verifier_node)  # type: ignore[arg-type]
    workflow.add_node("final_gate", final_gate_node)  # type: ignore[arg-type]
    workflow.add_node("reflection", reflection_node)  # type: ignore[arg-type]
    workflow.add_node("learner", learner_node)  # type: ignore[arg-type]

    # Imposta entry point
    workflow.set_entry_point("router")

    # Fase 2: router → clarify_or_expand (sempre, ma no-op se confidence alta).
    workflow.add_edge("router", "clarify_or_expand")

    # Dopo clarify_or_expand:
    #   - se ha emesso una richiesta di chiarimento → END (turno si ferma).
    #   - altrimenti → understanding (Cluster 2). Il nodo understanding e'
    #     pass-through se disabilitato/non complesso: in quel caso il routing
    #     verso planner/executor avviene comunque dopo, via route_after_router.
    def _route_after_clarify_or_expand(state: AgentState) -> str:
        if state.get("pending_clarify"):
            return "end"
        return "understanding"

    workflow.add_conditional_edges(
        "clarify_or_expand",
        _route_after_clarify_or_expand,
        {"end": END, "understanding": "understanding"},
    )
    # Dopo understanding (pass-through se OFF): routing standard planner/executor.
    workflow.add_conditional_edges(
        "understanding",
        route_after_router,
        {"planner": "planner", "executor": "executor"},
    )
    # Il planner emette il piano. Edge CONDIZIONALE (mig 0431):
    #   - isolamento todo attivo (Continuo + piano + setting ON) -> todo_runner,
    #     che esegue ogni todo come sub-run ISOLATA (context fresco, no accumulo).
    #   - altrimenti -> executor (comportamento storico INVARIANTE con setting OFF).
    workflow.add_conditional_edges(
        "planner",
        route_after_planner,
        {"todo_runner": "todo_runner", "executor": "executor"},
    )
    # Dopo todo_runner: re-entry per il prossimo todo, chiusura via final_gate/
    # learner, o fallback all'executor classico (guard no-op / dispatch fallito).
    workflow.add_conditional_edges(
        "todo_runner",
        route_after_todo_runner,
        {
            "todo_runner": "todo_runner",
            "final_gate": "final_gate",
            "executor": "executor",
            "learner": "reflection",
        },
    )

    # Dopo executor: loop su tool_dispatch, verificare (PR-2), o passo a reflection.
    # PR-2: se plan_phase_active + verifier_enabled e stop_reason=end_turn,
    # route_after_executor ritorna "verifier" che lancia la verifica DoD.
    workflow.add_conditional_edges(
        "executor",
        route_after_executor,
        {
            "tool_dispatch": "tool_dispatch",
            "verifier": "verifier",
            "final_gate": "final_gate",
            "learner": "reflection",
            "executor": "executor",
        },
    )
    # Final gate: se fallisce rimanda all'executor per l'integrazione,
    # altrimenti chiude verso reflection (-> learner).
    workflow.add_conditional_edges(
        "final_gate",
        route_after_final_gate,
        {"executor": "executor", "learner": "reflection"},
    )
    # tool_dispatch rientra nell'executor per un'altra iterazione.
    workflow.add_edge("tool_dispatch", "executor")
    # PR-2: dopo verifier, route_after_verifier decide se re-iterare (executor)
    # o chiudere (reflection -> learner).
    workflow.add_conditional_edges(
        "verifier",
        route_after_verifier,
        {"executor": "executor", "learner": "reflection"},
    )
    # reflection -> learner -> END: chiusura del run.
    workflow.add_edge("reflection", "learner")
    workflow.add_edge("learner", END)

    # Compila con checkpointer PostgreSQL asincrono
    # PostgresCheckpointer supporta solo operazioni asincrone (ainvoke()),
    # forzando l'utilizzo del paradigma asincrono che evita il deadlock con AsyncSqliteSaver.
    checkpointer = create_checkpointer()
    compile_kwargs: dict[str, Any] = {"checkpointer": checkpointer}

    # In modalita' agent-loop (tool_runner iniettato) non possiamo interrompere
    # prima di OGNI iterazione dell'executor: rompe il loop tool_use -> executor.
    # HITL resta disponibile via `/agent/approve` in modalita' legacy.
    if tool_runner is None:
        compile_kwargs["interrupt_before"] = ["executor"]

    compiled = workflow.compile(**compile_kwargs)

    logger.info(
        "Grafo LangGraph compilato: checkpointer=%s (PostgreSQL asincrono) learning=PostgreSQL",
        checkpointer.__class__.__name__,
    )

    return compiled
