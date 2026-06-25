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
pub mod final_gate;
pub mod learner;
pub mod reflection;
pub mod router;
pub mod understanding;

pub use clarify_or_expand::{
    ClarifyConfig, ClarifyMode, ClarifyOrExpandNode, DecisionCategory, GateOutcome, LlmDecision,
};
pub use final_gate::{count_build_errors, FinalGateConfig, FinalGateNode};
pub use learner::{LearnerConfig, LearnerNode, QdrantPayload};
pub use reflection::{ReflectionConfig, ReflectionData, ReflectionNode};
pub use router::RouterNode;
pub use understanding::{UnderstandingConfig, UnderstandingNode};
