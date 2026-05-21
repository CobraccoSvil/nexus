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
    route_by_task_type,
    router_node,
    tool_dispatch_node,
)
from .planner_node import planner_node, configure as _configure_planner
from .verifier_node import verifier_node, configure as _configure_verifier
from .clarify_or_expand_node import (
    clarify_or_expand_node,
    configure as _configure_clarify,
    route_after_clarify,
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
    eligible = orchestrator_config.is_eligible(behavior_mode, intent, token_budget)
    logger.info(
        "route_after_router: eligible=%s plan_enabled=%s mode=%r intent=%r budget=%d -> %s",
        eligible, cfg.get("plan_phase_enabled"), behavior_mode, intent, token_budget,
        "planner" if eligible else "executor",
    )
    return "planner" if eligible else "executor"


def create_agent_graph(
    providers: Any,
    router: Any,
    embeddings: Any,
    checkpointer_path: str | None = None,
    tool_runner: Any = None,
    agent_router: Any = None,
) -> Any:
    """Crea e compila il grafo LangGraph con tutti i servizi Nexus.

    Args:
        providers: ProviderRegistry globale di Nexus
        router: SemanticRouter globale di Nexus
        embeddings: EmbeddingService globale di Nexus
        checkpointer_path: Deprecato, ignorato. Il checkpointer usa PostgreSQL.
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
    _configure_verifier(tool_runner=tool_runner)
    _configure_clarify(providers=providers, routing_client=_routing_client)

    # Crea il grafo con lo schema di stato
    workflow: StateGraph = StateGraph(AgentState)

    # Aggiunge nodi
    workflow.add_node("router", router_node)  # type: ignore[arg-type]
    workflow.add_node("clarify_or_expand", clarify_or_expand_node)  # type: ignore[arg-type]
    workflow.add_node("planner", planner_node)  # type: ignore[arg-type]
    workflow.add_node("executor", executor_node)  # type: ignore[arg-type]
    workflow.add_node("tool_dispatch", tool_dispatch_node)  # type: ignore[arg-type]
    workflow.add_node("verifier", verifier_node)  # type: ignore[arg-type]
    workflow.add_node("reflection", reflection_node)  # type: ignore[arg-type]
    workflow.add_node("learner", learner_node)  # type: ignore[arg-type]

    # Imposta entry point
    workflow.set_entry_point("router")

    # Fase 2: router → clarify_or_expand (sempre, ma no-op se confidence alta).
    workflow.add_edge("router", "clarify_or_expand")

    # Dopo clarify_or_expand:
    #   - se ha emesso una richiesta di chiarimento → END (turno si ferma,
    #     l'utente risponde nel turno successivo).
    #   - altrimenti → route_after_router (planner o executor).
    def _route_after_clarify_or_expand(state: AgentState) -> str:
        if state.get("pending_clarify"):
            return "end"
        return route_after_router(state)

    workflow.add_conditional_edges(
        "clarify_or_expand",
        _route_after_clarify_or_expand,
        {"end": END, "planner": "planner", "executor": "executor"},
    )
    # Il planner emette il piano e poi passa il controllo all'executor.
    workflow.add_edge("planner", "executor")

    # Dopo executor: loop su tool_dispatch, verificare (PR-2), o passo a reflection.
    # PR-2: se plan_phase_active + verifier_enabled e stop_reason=end_turn,
    # route_after_executor ritorna "verifier" che lancia la verifica DoD.
    workflow.add_conditional_edges(
        "executor",
        route_after_executor,
        {
            "tool_dispatch": "tool_dispatch",
            "verifier": "verifier",
            "learner": "reflection",
        },
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
    # reflection passa sempre a learner (fire-and-forget interno).
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
