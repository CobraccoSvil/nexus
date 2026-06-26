//! Adapter del trait [`nexus_agent_graph::runtime::ports::VerifierRunStore`].
//!
//! IMPLEMENTERA' (FASE 2) `VerifierRunStore::record` con una INSERT best-effort su
//! `nexus_agent_verifier_runs` via `sqlx` (1:1 con
//! `verifier_node._persist_verifier_run`). La scrittura e' gata `Real` (no-op in
//! `ExecMode::Replay`, punto unico del gate shadow). Best-effort: su errore DB
//! l'impl logga e ritorna `Ok(())` (il `PortError` resta per un contratto rotto).

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use nexus_agent_graph::runtime::ports::{ExecMode, PortError, VerifierRunRecord, VerifierRunStore};

/// Adapter [`VerifierRunStore`] -> `nexus_agent_verifier_runs` via `sqlx`.
pub struct PgVerifierRunStore {
    /// Pool Postgres su cui gira la INSERT del verifier.
    db: PgPool,
}

impl PgVerifierRunStore {
    /// Costruisce lo store sul pool Postgres condiviso.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait]
impl VerifierRunStore for PgVerifierRunStore {
    /// INSERT best-effort su `nexus_agent_verifier_runs` (1:1 con
    /// `verifier_node._persist_verifier_run`, stessa colonna-list `run_id, todo_id,
    /// cycle, criteria_results, passed, duration_ms`).
    ///
    /// Gate shadow (regola L): no-op in [`ExecMode::Replay`] (il run shadow non
    /// inquina la telemetria del primario). Best-effort come il Python: un errore
    /// di INSERT (run_id/todo_id non parseabili a UUID, FK assente, DB down) e'
    /// loggato e ritorna `Ok(())`; il `PortError` resta per un contratto rotto, mai
    /// usato nel flusso normale.
    async fn record(&self, run: VerifierRunRecord, mode: ExecMode) -> Result<(), PortError> {
        if mode != ExecMode::Real {
            return Ok(());
        }
        // run_id/todo_id sono UUID a livello DB (FK su nexus_agent_plans/_todos):
        // se non parseano la riga e' incoerente -> best-effort skip con WARN.
        let run_uuid = match Uuid::parse_str(&run.run_id) {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!(
                    run_id = %run.run_id,
                    error = %e,
                    "verifier_run_store: run_id non e' un UUID, INSERT saltata"
                );
                return Ok(());
            }
        };
        let todo_uuid = match Uuid::parse_str(&run.todo_id) {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!(
                    todo_id = %run.todo_id,
                    error = %e,
                    "verifier_run_store: todo_id non e' un UUID, INSERT saltata"
                );
                return Ok(());
            }
        };
        let res = sqlx::query(
            "INSERT INTO nexus_agent_verifier_runs \
             (run_id, todo_id, cycle, criteria_results, passed, duration_ms) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(run_uuid)
        .bind(todo_uuid)
        .bind(run.cycle)
        .bind(&run.criteria_results)
        .bind(run.passed)
        .bind(run.duration_ms)
        .execute(&self.db)
        .await;
        if let Err(e) = res {
            tracing::warn!(
                run_id = %run.run_id,
                todo_id = %run.todo_id,
                error = %e,
                "verifier_run_store: INSERT nexus_agent_verifier_runs fallita (best-effort)"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Tabella minimale senza FK strette (il gate/best-effort si testa cosi' senza
    /// dover ricostruire l'intero schema plans/todos).
    async fn create_table(pool: &PgPool) {
        sqlx::query(
            "CREATE TABLE nexus_agent_verifier_runs ( \
                 id UUID NOT NULL DEFAULT gen_random_uuid(), \
                 run_id UUID NOT NULL, \
                 todo_id UUID, \
                 cycle INTEGER NOT NULL, \
                 criteria_results JSONB NOT NULL, \
                 passed BOOLEAN NOT NULL, \
                 duration_ms INTEGER \
             )",
        )
        .execute(pool)
        .await
        .expect("create table verifier_runs");
    }

    fn record(run_id: Uuid, todo_id: Uuid) -> VerifierRunRecord {
        VerifierRunRecord {
            run_id: run_id.to_string(),
            todo_id: todo_id.to_string(),
            cycle: 1,
            criteria_results: json!([{"type": "outputs_exist", "passed": true}]),
            passed: true,
            duration_ms: 42,
        }
    }

    #[sqlx::test]
    async fn real_inserisce(pool: PgPool) {
        create_table(&pool).await;
        let store = PgVerifierRunStore::new(pool.clone());
        let run_id = Uuid::new_v4();
        let todo_id = Uuid::new_v4();
        store
            .record(record(run_id, todo_id), ExecMode::Real)
            .await
            .expect("record ok");
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM nexus_agent_verifier_runs WHERE run_id = $1")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .expect("count");
        assert_eq!(count, 1, "in Real la INSERT avviene");
    }

    #[sqlx::test]
    async fn replay_e_no_op(pool: PgPool) {
        create_table(&pool).await;
        let store = PgVerifierRunStore::new(pool.clone());
        let run_id = Uuid::new_v4();
        store
            .record(record(run_id, Uuid::new_v4()), ExecMode::Replay)
            .await
            .expect("record ok");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nexus_agent_verifier_runs")
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 0, "in Replay (shadow) NESSUNA scrittura");
    }

    #[sqlx::test]
    async fn run_id_non_uuid_e_best_effort(pool: PgPool) {
        create_table(&pool).await;
        let store = PgVerifierRunStore::new(pool.clone());
        let rec = VerifierRunRecord {
            run_id: "non-un-uuid".to_string(),
            todo_id: Uuid::new_v4().to_string(),
            cycle: 1,
            criteria_results: json!([]),
            passed: false,
            duration_ms: 0,
        };
        // Best-effort: ritorna Ok senza inserire e senza errore propagato.
        store.record(rec, ExecMode::Real).await.expect("best-effort Ok");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nexus_agent_verifier_runs")
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 0);
    }
}
