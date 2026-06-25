//! Persistenza dello stato del grafo (punto unico, regola L).
//!
//! Il trait astrae il backing store: il runtime non sa se i checkpoint vanno su
//! Postgres, in memoria o altrove. L'implementazione concreta su Postgres
//! (`PgCheckpointer`) vive in `nexus-agent-graph`. In FASE 0 il motore usa un
//! `MemoryCheckpointer` per i test (nessuna dipendenza dal DB).
//!
//! `superstep` e' un BIGINT monotono (non un id random): il resume e'
//! deterministico ("riprendi dall'ultimo superstep completo"). Il checkpoint
//! registra anche `next_node`, cioe' il puntatore di esecuzione esplicito (in
//! LangGraph era implicito nei `channel_versions`).

use async_trait::async_trait;
use thiserror::Error;
use uuid::Uuid;

use crate::node::NodeId;

/// Errore del checkpointer. Generico rispetto al backend (il dettaglio del
/// backend e' un messaggio + un eventuale errore sorgente).
#[derive(Debug, Error)]
pub enum CheckpointError {
    /// Errore del backing store (DB, IO, serializzazione).
    #[error("checkpoint store: {0}")]
    Store(String),

    /// Il `next_node` salvato non corrisponde a nessuna variante di `NodeId`
    /// (schema/dato incoerente).
    #[error("next_node sconosciuto nel checkpoint: '{0}'")]
    UnknownNode(String),
}

/// Persistenza dello stato del grafo.
///
/// `S` deve essere serializzabile dall'implementazione concreta (qui il trait
/// resta agnostico: i vincoli serde vivono sull'impl `PgCheckpointer`).
#[async_trait]
pub trait Checkpointer<S>: Send + Sync {
    /// Salva uno snapshot DOPO il route: il record contiene gia' il prossimo
    /// nodo da eseguire (`next`), cosi' il resume riparte da li' senza ricalcolo.
    async fn put(
        &self,
        run_id: Uuid,
        superstep: i64,
        next: NodeId,
        state: &S,
    ) -> Result<(), CheckpointError>;

    /// Carica l'ultimo checkpoint (superstep massimo) di un run: ritorna lo
    /// stato e il nodo da cui riprendere. `None` se il run non ha checkpoint.
    async fn load(&self, run_id: Uuid) -> Result<Option<(S, NodeId)>, CheckpointError>;
}
