//! `nexus-agent-graph` — nodi concreti Nexus + checkpointer Postgres.
//!
//! Istanzia il runtime puro `nexus-graph` con i 12 nodi reali del grafo
//! agentico Nexus. In FASE 0 (scaffold) contiene il `PgCheckpointer`
//! (persistenza su `nexus_graph_checkpoints`, migrazione 0451); in FASE 1 lo
//! stato tipizzato (`AgentState` + reducer derivato + `StateDelta`). In FASE 3
//! l'INFRASTRUTTURA dei nodi I/O (modulo `runtime`: porte astratte +
//! `AgentNodeCtx`), il primo nodo (`nodes::RouterNode`) e lo scaffold della
//! modalita' shadow (`shadow`, telemetria mig 0453). Il transport SSE e i nodi
//! restanti arrivano nelle fasi successive del porting (vedi
//! `/tmp/langgraph_plan.md`).
//!
//! VINCOLO ARCHITETTURALE: questo crate NON dipende da `mcp-core` (mcp-core
//! dipende da lui). Le dipendenze I/O sono trait astratti in `runtime::ports`;
//! mcp-core li implementera' in un PR futuro (inversione di dipendenza).

pub mod checkpoint_pg;
pub mod decisions;
#[cfg(test)]
pub mod golden_util;
pub mod graph;
pub mod nodes;
pub mod py_json;
pub mod routing;
pub mod runtime;
pub mod shadow;
pub mod state;

pub use checkpoint_pg::PgCheckpointer;
pub use graph::{
    build_agent_graph, node_target_to_node_id, AgentGraphEngine, AgentGraphNode, AgentGraphNodes,
};
pub use nodes::{
    build_error_files, clarifying_branch, count_build_errors, error_file_matches_touched,
    plan_reuse_decision, ClarifyConfig, ClarifyMode, ClarifyOrExpandNode, ClarifyingBranch,
    DecisionCategory, FinalGateConfig, FinalGateNode, GateOutcome, LearnerNode,
    ReviewGateConfig, ReviewGateNode,
    LlmDecision, OnFailure, PlanReuse, PlannerConfig, PlannerNode, ReflectionConfig,
    ReflectionData, ReflectionNode, RouterNode, TodoRunnerConfig, TodoRunnerNode,
    ToolDispatchConfig, ToolDispatchNode, ToolResultOutcome, UnderstandingConfig,
    UnderstandingNode,
};
pub use runtime::{
    AgentNodeCtx, CriteriaRunner, CriterionResult, CriterionSpec, EventSink, ExecMode, LlmGateway,
    PlanRow, TodoStore, ToolExecutor,
};
pub use shadow::{compute_diff, persist_node_diff, DiffCollector, NodeDiff};
pub use state::{
    AgentState, AutomationMode, FinalGateVerdict, Message, MetaStep, ReviewGateVerdict, StateDelta, StopReason,
    SupervisorMode, TaskComplexity,
};
