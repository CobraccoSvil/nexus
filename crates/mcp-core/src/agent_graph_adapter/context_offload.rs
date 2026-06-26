//! Adapter del trait [`nexus_agent_graph::runtime::ports::ContextOffload`].
//!
//! IMPLEMENTERA' (FASE 2) `ContextOffload::offload_to_rag` scrivendo il payload su
//! RAG (Qdrant + embeddings) e ritornando un pointer opaco, delegando
//! all'infrastruttura vector concreta di mcp-core ([`crate::vector_memory`] +
//! `ruvector`). La logica head/tail/pointer-size (DECIDERE cosa offloadare) resta
//! PURA fuori da qui (regola L): questo adapter isola SOLO l'I/O di offload. Gata
//! `Real` (no-op che ritorna `PortError` in `ExecMode::Replay`); su guasto infra il
//! chiamante degrada a troncamento testa+coda. Il pool serve la persistenza dei
//! metadati di offload; il client RAG/embed concreto verra' cablato in F2.

use sqlx::PgPool;

/// Adapter [`ContextOffload`] -> RAG (Qdrant + embeddings) via `vector_memory`.
///
/// F2 implementera' il trait `ContextOffload` su questa struct.
pub struct RagContextOffloadAdapter {
    /// Pool Postgres per i metadati di offload; F2 affianchera' il client
    /// RAG/embed concreto per la scrittura su Qdrant.
    db: PgPool,
}

impl RagContextOffloadAdapter {
    /// Costruisce l'adapter sul pool Postgres condiviso.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}
