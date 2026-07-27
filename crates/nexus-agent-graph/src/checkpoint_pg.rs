//! Checkpointer Postgres (punto unico della persistenza del grafo, regola L).
//!
//! Persiste gli snapshot di stato per-superstep sulla tabella
//! `nexus_graph_checkpoints` (migrazione versionata 0451: chiude la violazione
//! H del `CREATE TABLE IF NOT EXISTS` runtime che esisteva nel checkpointer
//! Python). Riusa il `PgPool` condiviso di mcp-core: NESSUNA connection string
//! hardcoded (regola G — il vecchio `checkpointer.py` aveva
//! `postgresql://nexus:nexus@localhost:5433/nexus` inline).
//!
//! Lo stato e' serializzato con `serde_json` (JSONB), NON con
//! `langchain_core.dumps`: il formato e' lo struct Rust round-trippabile, non il
//! `{lc,type,id,kwargs}` di LangChain.

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use nexus_graph::checkpoint::{CheckpointError, Checkpointer};
use nexus_graph::node::NodeId;

/// Persistenza dei checkpoint del grafo su Postgres.
///
/// Generico sullo stato `S`, che deve essere serde-serializzabile. Clona a
/// basso costo (il `PgPool` e' un `Arc` internamente).
#[derive(Clone)]
pub struct PgCheckpointer {
    pool: PgPool,
}

impl PgCheckpointer {
    /// Costruisce il checkpointer riusando il pool condiviso del processo.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Mappa un errore sqlx nell'errore generico del checkpointer (regola H: non
/// ingoiare, conserva il dettaglio nel messaggio).
fn store_err(e: sqlx::Error) -> CheckpointError {
    CheckpointError::Store(e.to_string())
}

#[async_trait]
impl<S> Checkpointer<S> for PgCheckpointer
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
        let state_json =
            serde_json::to_value(state).map_err(|e| CheckpointError::Store(e.to_string()))?;

        // UPSERT su (run_id, superstep): un re-run dello stesso superstep
        // sovrascrive (idempotente). `next_node` e' il puntatore di esecuzione
        // esplicito salvato come TEXT.
        sqlx::query(
            "INSERT INTO nexus_graph_checkpoints (run_id, superstep, next_node, state) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (run_id, superstep) \
             DO UPDATE SET next_node = EXCLUDED.next_node, state = EXCLUDED.state",
        )
        .bind(run_id)
        .bind(superstep)
        .bind(next.as_label())
        .bind(state_json)
        .execute(&self.pool)
        .await
        .map_err(store_err)?;

        Ok(())
    }

    async fn load(&self, run_id: Uuid) -> Result<Option<(S, NodeId)>, CheckpointError> {
        // Ultimo checkpoint = superstep massimo (monotono): resume deterministico.
        let row = sqlx::query_as::<_, (serde_json::Value, String)>(
            "SELECT state, next_node FROM nexus_graph_checkpoints \
             WHERE run_id = $1 ORDER BY superstep DESC LIMIT 1",
        )
        .bind(run_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_err)?;

        match row {
            None => Ok(None),
            Some((state_json, next_label)) => {
                let state: S = serde_json::from_value(state_json)
                    .map_err(|e| CheckpointError::Store(e.to_string()))?;
                let next = NodeId::from_label(&next_label)
                    .ok_or(CheckpointError::UnknownNode(next_label))?;
                Ok(Some((state, next)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    /// Stato minimale di prova: serde-serializzabile, basta per il round-trip.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct ProbeState {
        intent: String,
        iterations: i64,
        done: bool,
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::PROJECT_MIGRATOR")]
    async fn put_poi_load_ritorna_stato_identico(pool: PgPool) {
        let cp = PgCheckpointer::new(pool);
        let run_id = Uuid::new_v4();
        let state = ProbeState {
            intent: "code_fix".to_string(),
            iterations: 3,
            done: false,
        };

        cp.put(run_id, 0, NodeId::Executor, &state)
            .await
            .expect("put deve riuscire");

        let (loaded, next) = Checkpointer::<ProbeState>::load(&cp, run_id)
            .await
            .expect("load deve riuscire")
            .expect("il run ha un checkpoint");

        assert_eq!(loaded, state, "lo stato caricato deve essere identico");
        assert_eq!(next, NodeId::Executor, "il next_node deve round-trippare");
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::PROJECT_MIGRATOR")]
    async fn load_ritorna_il_superstep_massimo(pool: PgPool) {
        let cp = PgCheckpointer::new(pool);
        let run_id = Uuid::new_v4();

        let s0 = ProbeState {
            intent: "a".to_string(),
            iterations: 0,
            done: false,
        };
        let s1 = ProbeState {
            intent: "b".to_string(),
            iterations: 1,
            done: true,
        };

        cp.put(run_id, 0, NodeId::Router, &s0).await.expect("put 0");
        cp.put(run_id, 1, NodeId::End, &s1).await.expect("put 1");

        let (loaded, next) = Checkpointer::<ProbeState>::load(&cp, run_id)
            .await
            .expect("load")
            .expect("checkpoint presente");
        assert_eq!(loaded, s1, "deve caricare l'ultimo superstep");
        assert_eq!(next, NodeId::End);
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::PROJECT_MIGRATOR")]
    async fn load_su_run_senza_checkpoint_ritorna_none(pool: PgPool) {
        let cp = PgCheckpointer::new(pool);
        let loaded: Option<(ProbeState, NodeId)> = cp
            .load(Uuid::new_v4())
            .await
            .expect("load non deve errorare");
        assert!(loaded.is_none());
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::PROJECT_MIGRATOR")]
    async fn put_idempotente_sullo_stesso_superstep(pool: PgPool) {
        let cp = PgCheckpointer::new(pool);
        let run_id = Uuid::new_v4();

        let s0 = ProbeState {
            intent: "primo".to_string(),
            iterations: 0,
            done: false,
        };
        let s0_bis = ProbeState {
            intent: "secondo".to_string(),
            iterations: 0,
            done: true,
        };

        cp.put(run_id, 0, NodeId::Router, &s0)
            .await
            .expect("put iniziale");
        // Stesso superstep: deve sovrascrivere (UPSERT), non fallire sul PK.
        cp.put(run_id, 0, NodeId::Planner, &s0_bis)
            .await
            .expect("put deve essere idempotente sullo stesso superstep");

        let (loaded, next) = Checkpointer::<ProbeState>::load(&cp, run_id)
            .await
            .expect("load")
            .expect("presente");
        assert_eq!(loaded, s0_bis);
        assert_eq!(next, NodeId::Planner);
    }
}
