//! Nodi concreti del grafo agentico Nexus.
//!
//! Ogni nodo implementa `nexus_graph::node::GraphNode<AgentState, AgentNodeCtx>`
//! (RIUSO del trait esistente, regola L: nessun trait nuovo tipo "AsyncNode").
//! Il tipo di stato `S` e' `AgentState`, il contesto `C` e' `AgentNodeCtx`
//! (porte I/O astratte + DB + config). I nodi NON instradano: l'edge e'
//! dichiarato fuori dal nodo (vedi `nexus-graph::edge`).
//!
//! In QUESTO PR sono portati `RouterNode` (caso passthrough/deterministico),
//! `UnderstandingNode` (Cluster 2, comprensione pre-planning) e
//! `ClarifyOrExpandNode` (gate di disambiguazione, rami ask/expand); i restanti
//! nodi reali arrivano nei PR successivi del porting.

pub mod clarify_or_expand;
pub mod executor;
pub mod final_gate;
pub mod learner;
pub mod planner;
pub mod reflection;
pub mod router;
pub mod todo_runner;
pub mod tool_dispatch;
pub mod understanding;
pub mod verifier;

pub use clarify_or_expand::{
    ClarifyConfig, ClarifyMode, ClarifyOrExpandNode, DecisionCategory, GateOutcome, LlmDecision,
};
pub use executor::{ExecutorConfig, ExecutorNode};
pub use final_gate::{count_build_errors, FinalGateConfig, FinalGateNode};
pub use learner::{LearnerConfig, LearnerNode, QdrantPayload};
pub use planner::{
    clarifying_branch, plan_reuse_decision, ClarifyingBranch, PlanReuse, PlannerConfig, PlannerNode,
    ToolResultOutcome,
};
pub use reflection::{ReflectionConfig, ReflectionData, ReflectionNode};
pub use router::RouterNode;
pub use todo_runner::{OnFailure, TodoRunnerConfig, TodoRunnerNode};
pub use tool_dispatch::{ToolDispatchConfig, ToolDispatchNode};
pub use understanding::{UnderstandingConfig, UnderstandingNode};
pub use verifier::{suggest_remediation, VerifierConfig, VerifierNode};
