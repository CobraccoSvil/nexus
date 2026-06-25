//! Nodi concreti del grafo agentico Nexus.
//!
//! Ogni nodo implementa `nexus_graph::node::GraphNode<AgentState, AgentNodeCtx>`
//! (RIUSO del trait esistente, regola L: nessun trait nuovo tipo "AsyncNode").
//! Il tipo di stato `S` e' `AgentState`, il contesto `C` e' `AgentNodeCtx`
//! (porte I/O astratte + DB + config). I nodi NON instradano: l'edge e'
//! dichiarato fuori dal nodo (vedi `nexus-graph::edge`).
//!
//! In QUESTO PR sono portati `RouterNode` (caso passthrough/deterministico) e
//! `UnderstandingNode` (Cluster 2, comprensione pre-planning); i restanti nodi
//! reali arrivano nei PR successivi del porting.

pub mod router;
pub mod understanding;

pub use router::RouterNode;
pub use understanding::{UnderstandingConfig, UnderstandingNode};
