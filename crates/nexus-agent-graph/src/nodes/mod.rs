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
pub mod scale_control;
pub mod stall_recovery;
pub mod todo_runner;
pub mod tool_dispatch;
pub mod understanding;
pub mod verifier;

pub use clarify_or_expand::{
    ClarifyConfig, ClarifyMode, ClarifyOrExpandNode, DecisionCategory, GateOutcome, LlmDecision,
};
pub use executor::{ExecutorConfig, ExecutorNode, ScaleConfig};
pub use final_gate::{count_build_errors, FinalGateConfig, FinalGateNode};
pub use learner::{LearnerConfig, LearnerNode, QdrantPayload};
pub use planner::{
    clarifying_branch, plan_reuse_decision, ClarifyingBranch, PlanReuse, PlannerConfig, PlannerNode,
    ToolResultOutcome,
};
pub use reflection::{ReflectionConfig, ReflectionData, ReflectionNode};
pub use router::RouterNode;
pub use scale_control::{ScaleControlNode, SCALE_CONTEXT_KEY, SCALE_MOVE_CACHE_KEY_KEY};
pub use stall_recovery::{stall_move_key, StallRecoveryNode, STALL_CONTEXT_KEY};
pub use todo_runner::{OnFailure, TodoRunnerConfig, TodoRunnerNode};
pub use tool_dispatch::{ToolDispatchConfig, ToolDispatchNode};
pub use understanding::{UnderstandingConfig, UnderstandingNode};

/// NARRAZIONE LIVE di una FASE semantica del run (punto unico, regola L):
/// emette il meta-step verso la chat (`EventSink`, no-op in shadow) e lo
/// persiste per il ripristino post-reload (`MetaStepStore`, gata Real).
/// Best-effort: non fallisce mai il turno. Usato da executor / tool_dispatch /
/// final_gate / planner — i due canali (live vs storico) restano trait separati
/// per contratto (vedi doc di `MetaStepStore`), qui si compongono UNA volta.
pub(crate) async fn emit_phase_meta(
    emit: &dyn crate::runtime::ports::EventSink,
    store: &dyn crate::runtime::ports::MetaStepStore,
    mode: crate::runtime::ports::ExecMode,
    kind: &str,
    title: String,
    payload: serde_json::Value,
) {
    emit.emit(crate::runtime::ports::SseEvent::MetaStep {
        kind: kind.to_string(),
        title: title.clone(),
        payload: payload.clone(),
    });
    let meta = serde_json::json!({"kind": kind, "title": title, "payload": payload});
    let _ = store.persist_meta_step(meta, mode).await;
}
pub use verifier::{suggest_remediation, VerifierConfig, VerifierNode};
