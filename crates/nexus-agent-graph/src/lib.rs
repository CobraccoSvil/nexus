//! `nexus-agent-graph` — nodi concreti Nexus + checkpointer Postgres.
//!
//! Istanzia il runtime puro `nexus-graph` con i 12 nodi reali del grafo
//! agentico Nexus. In FASE 0 (scaffold) contiene solo il `PgCheckpointer`
//! (persistenza su `nexus_graph_checkpoints`, migrazione 0451); lo stato
//! tipizzato (`AgentState`), i nodi reali e il transport SSE arrivano nelle fasi
//! successive del porting (vedi `/tmp/langgraph_plan.md`).

pub mod checkpoint_pg;

pub use checkpoint_pg::PgCheckpointer;
