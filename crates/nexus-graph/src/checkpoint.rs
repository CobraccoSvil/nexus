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

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Mutex;

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::Serialize;
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

/// Checkpointer IN-MEMORY generico (niente DB, niente IO).
///
/// Punto unico del checkpointer volatile (regola L): usato dai test del motore
/// e — soprattutto — dal run SHADOW in produzione. Lo shadow gira UNA volta fino
/// a `End` e non ha bisogno di resume persistente; deve pero' NON scrivere su
/// `nexus_graph_checkpoints` (i checkpoint Python e Rust hanno topologie diverse
/// e non sono interscambiabili: persisterli inquinerebbe la tabella del recovery
/// del primario). Questo store tiene gli snapshot in una `HashMap` interna e li
/// scarta a fine vita dell'istanza.
///
/// `S` deve essere serde-round-trippabile (stesso vincolo del [`crate::checkpoint_pg`]
/// Postgres in `nexus-agent-graph`): lo stato e' serializzato a `Value` e
/// deserializzato al `load`, cosi' il comportamento e' identico al backend reale.
pub struct MemoryCheckpointer<S> {
    /// (run_id, superstep) -> (stato serializzato, label del next_node).
    store: Mutex<HashMap<(Uuid, i64), (serde_json::Value, &'static str)>>,
    /// Marcatore del tipo di stato (lo store conserva `Value`, non `S`).
    _state: PhantomData<fn() -> S>,
}

impl<S> Default for MemoryCheckpointer<S> {
    fn default() -> Self {
        Self {
            store: Mutex::new(HashMap::new()),
            _state: PhantomData,
        }
    }
}

impl<S> MemoryCheckpointer<S> {
    /// Crea un checkpointer in-memory vuoto.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl<S> Checkpointer<S> for MemoryCheckpointer<S>
where
    S: Serialize + DeserializeOwned + Send + Sync,
{
    async fn put(
        &self,
        run_id: Uuid,
        superstep: i64,
        next: NodeId,
        state: &S,
    ) -> Result<(), CheckpointError> {
        let json =
            serde_json::to_value(state).map_err(|e| CheckpointError::Store(e.to_string()))?;
        self.store
            .lock()
            .map_err(|e| CheckpointError::Store(format!("mutex avvelenato: {e}")))?
            .insert((run_id, superstep), (json, next.as_label()));
        Ok(())
    }

    async fn load(&self, run_id: Uuid) -> Result<Option<(S, NodeId)>, CheckpointError> {
        let guard = self
            .store
            .lock()
            .map_err(|e| CheckpointError::Store(format!("mutex avvelenato: {e}")))?;
        let latest = guard
            .iter()
            .filter(|((rid, _), _)| *rid == run_id)
            .max_by_key(|((_, step), _)| *step);
        match latest {
            None => Ok(None),
            Some((_, (json, label))) => {
                let state: S = serde_json::from_value(json.clone())
                    .map_err(|e| CheckpointError::Store(e.to_string()))?;
                let node = NodeId::from_label(label)
                    .ok_or_else(|| CheckpointError::UnknownNode((*label).to_string()))?;
                Ok(Some((state, node)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct ProbeState {
        intent: String,
        iterations: i64,
    }

    #[tokio::test]
    async fn memory_checkpointer_put_load_round_trip() {
        let cp = MemoryCheckpointer::<ProbeState>::new();
        let run = Uuid::new_v4();
        let s = ProbeState {
            intent: "code_fix".to_string(),
            iterations: 2,
        };
        cp.put(run, 0, NodeId::Executor, &s).await.expect("put");
        let (loaded, next) = cp.load(run).await.expect("load").expect("presente");
        assert_eq!(loaded, s);
        assert_eq!(next, NodeId::Executor);
    }

    #[tokio::test]
    async fn memory_checkpointer_carica_superstep_massimo() {
        let cp = MemoryCheckpointer::<ProbeState>::new();
        let run = Uuid::new_v4();
        let s0 = ProbeState {
            intent: "a".to_string(),
            iterations: 0,
        };
        let s1 = ProbeState {
            intent: "b".to_string(),
            iterations: 1,
        };
        cp.put(run, 0, NodeId::Router, &s0).await.expect("put 0");
        cp.put(run, 1, NodeId::End, &s1).await.expect("put 1");
        let (loaded, next) = cp.load(run).await.expect("load").expect("presente");
        assert_eq!(loaded, s1, "ultimo superstep");
        assert_eq!(next, NodeId::End);
    }

    #[tokio::test]
    async fn memory_checkpointer_run_senza_checkpoint_e_none() {
        let cp = MemoryCheckpointer::<ProbeState>::new();
        let loaded = cp.load(Uuid::new_v4()).await.expect("load");
        assert!(loaded.is_none());
    }
}
