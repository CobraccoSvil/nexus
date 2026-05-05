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
    route_by_task_type,
    router_node,
    tool_dispatch_node,
)
from .state import AgentState

logger = logging.getLogger(__name__)


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
        checkpointer_path: Path opzionale al database SQLite per il checkpointer.
                           Se None usa il path default in nexus_memory/langgraph.db.

    Returns:
        Grafo compilato con SqliteSaver e interrupt_before=["executor"].
    """
    from brain.memory.retrieval import InteractionRetriever
    from brain.memory.storage import LocalLearningStorage

    from .checkpointer import create_checkpointer, get_memory_db_path

    # Inizializza storage locale
    memory_path = get_memory_db_path()
    storage = LocalLearningStorage(db_path=memory_path)
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

    # Crea il grafo con lo schema di stato
    workflow: StateGraph = StateGraph(AgentState)

    # Aggiunge nodi
    workflow.add_node("router", router_node)  # type: ignore[arg-type]
    workflow.add_node("executor", executor_node)  # type: ignore[arg-type]
    workflow.add_node("tool_dispatch", tool_dispatch_node)  # type: ignore[arg-type]
    workflow.add_node("reflection", reflection_node)  # type: ignore[arg-type]
    workflow.add_node("learner", learner_node)  # type: ignore[arg-type]

    # Imposta entry point
    workflow.set_entry_point("router")

    # Routing condizionale da router a executor
    workflow.add_conditional_edges(
        "router",
        route_by_task_type,
        {"executor": "executor"},
    )

    # Dopo executor: loop su tool_dispatch o passo a reflection.
    # Nota: route_after_executor ora manda a "reflection" invece di "learner"
    # direttamente. reflection_node poi passa sempre a learner.
    workflow.add_conditional_edges(
        "executor",
        route_after_executor,
        {"tool_dispatch": "tool_dispatch", "learner": "reflection"},
    )
    # tool_dispatch rientra nell'executor per un'altra iterazione.
    workflow.add_edge("tool_dispatch", "executor")
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
        "Grafo LangGraph compilato: checkpointer=%s (PostgreSQL asincrono) memory=%s",
        checkpointer.__class__.__name__,
        memory_path,
    )

    return compiled
