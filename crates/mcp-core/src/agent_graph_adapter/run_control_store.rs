//! Adapter del trait [`nexus_agent_graph::runtime::ports::RunControlStore`].
//!
//! IMPLEMENTERA' (FASE 2) il controllo di run condiviso da `executor` e
//! `tool_dispatch` (PUNTO UNICO, regola L) su `agent_runs` via `sqlx`:
//! - `is_superseded` (lettura del flag `superseded`/`supersede_active_runs`,
//!   FAIL-OPEN: errore DB -> `Ok(false)`, il run prosegue);
//! - `heartbeat` (UPDATE `updated_at` best-effort, gata `Real`);
//! - `set_effective_model` (registra provider/model effettivi dal gateway, gata
//!   `Real`).
//! Le scritture sono no-op in `ExecMode::Replay` (il run shadow non tocca la
//! telemetria del primario).

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use nexus_agent_graph::runtime::ports::{ExecMode, PortError, RunControlStore};

/// Adapter [`RunControlStore`] -> `agent_runs` via `sqlx`.
pub struct PgRunControlStore {
    /// Pool Postgres su cui girano le letture/UPDATE del controllo run.
    db: PgPool,
}

impl PgRunControlStore {
    /// Costruisce lo store sul pool Postgres condiviso.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait]
impl RunControlStore for PgRunControlStore {
    /// `true` se il run e' stato superato (last-wins). Usa lo STESSO segnale che
    /// `supersede_active_runs` scrive (`cancellation_requested=NOW()` quando marca
    /// 'cancelled') e che il brain interroga via `_check_superseded` (regola L:
    /// niente predicato duplicato). Un run con `cancellation_requested IS NOT NULL`
    /// deve fermarsi.
    ///
    /// FAIL-OPEN (regola di sicurezza): run_id non-UUID o errore di lettura ->
    /// `Ok(false)` (il run PROSEGUE), mai un `PortError` che lo bloccherebbe per un
    /// guasto infrastrutturale.
    async fn is_superseded(&self, run_id: &str) -> Result<bool, PortError> {
        let run_uuid = match Uuid::parse_str(run_id) {
            Ok(u) => u,
            Err(_) => return Ok(false),
        };
        let res = sqlx::query_scalar::<_, bool>(
            "SELECT (cancellation_requested IS NOT NULL) FROM agent_runs WHERE id = $1",
        )
        .bind(run_uuid)
        .fetch_optional(&self.db)
        .await;
        match res {
            Ok(Some(superseded)) => Ok(superseded),
            // Run inesistente: non superato (prosegue), coerente col fail-open.
            Ok(None) => Ok(false),
            Err(e) => {
                tracing::warn!(
                    run_id = %run_id,
                    error = %e,
                    "run_control_store: is_superseded query fallita, fail-open false"
                );
                Ok(false)
            }
        }
    }

    /// Heartbeat di liveness: UPDATE `agent_runs.updated_at = NOW()` (mig 0392,
    /// anti-recovery prematuro). Gata `Real` (no-op in shadow). Best-effort.
    async fn heartbeat(&self, run_id: &str, mode: ExecMode) -> Result<(), PortError> {
        if mode != ExecMode::Real {
            return Ok(());
        }
        let run_uuid = match Uuid::parse_str(run_id) {
            Ok(u) => u,
            Err(_) => return Ok(()),
        };
        let res = sqlx::query("UPDATE agent_runs SET updated_at = NOW() WHERE id = $1")
            .bind(run_uuid)
            .execute(&self.db)
            .await;
        if let Err(e) = res {
            tracing::warn!(
                run_id = %run_id,
                error = %e,
                "run_control_store: heartbeat fallito (best-effort)"
            );
        }
        Ok(())
    }

    /// Registra provider/model EFFETTIVAMENTE usati dal gateway sul run (la chat
    /// mostra il modello reale, non quello richiesto). Riusa lo stesso UPDATE
    /// provider/model di `agent_run.rs` (regola L). Gata `Real` (no-op in shadow).
    /// Best-effort.
    async fn set_effective_model(
        &self,
        run_id: &str,
        provider: &str,
        model: &str,
        mode: ExecMode,
    ) -> Result<(), PortError> {
        if mode != ExecMode::Real {
            return Ok(());
        }
        let run_uuid = match Uuid::parse_str(run_id) {
            Ok(u) => u,
            Err(_) => return Ok(()),
        };
        let res = sqlx::query("UPDATE agent_runs SET provider = $1, model = $2 WHERE id = $3")
            .bind(provider)
            .bind(model)
            .bind(run_uuid)
            .execute(&self.db)
            .await;
        if let Err(e) = res {
            tracing::warn!(
                run_id = %run_id,
                error = %e,
                "run_control_store: set_effective_model fallito (best-effort)"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn create_table(pool: &PgPool) {
        sqlx::query(
            "CREATE TABLE agent_runs ( \
                 id UUID PRIMARY KEY, \
                 provider TEXT, \
                 model TEXT, \
                 updated_at TIMESTAMPTZ, \
                 cancellation_requested TIMESTAMPTZ \
             )",
        )
        .execute(pool)
        .await
        .expect("create agent_runs");
    }

    async fn insert_run(pool: &PgPool, superseded: bool) -> Uuid {
        let id = Uuid::new_v4();
        let sql = if superseded {
            "INSERT INTO agent_runs (id, cancellation_requested) VALUES ($1, NOW())"
        } else {
            "INSERT INTO agent_runs (id) VALUES ($1)"
        };
        sqlx::query(sql).bind(id).execute(pool).await.expect("insert");
        id
    }

    #[sqlx::test]
    async fn is_superseded_vero_quando_cancellation_requested(pool: PgPool) {
        create_table(&pool).await;
        let store = PgRunControlStore::new(pool.clone());
        let run_id = insert_run(&pool, true).await;
        assert!(store.is_superseded(&run_id.to_string()).await.expect("ok"));
    }

    #[sqlx::test]
    async fn is_superseded_falso_run_attivo(pool: PgPool) {
        create_table(&pool).await;
        let store = PgRunControlStore::new(pool.clone());
        let run_id = insert_run(&pool, false).await;
        assert!(!store.is_superseded(&run_id.to_string()).await.expect("ok"));
    }

    #[sqlx::test]
    async fn is_superseded_fail_open_run_inesistente_e_uuid_invalido(pool: PgPool) {
        create_table(&pool).await;
        let store = PgRunControlStore::new(pool.clone());
        // Run inesistente -> false (prosegue).
        assert!(!store
            .is_superseded(&Uuid::new_v4().to_string())
            .await
            .expect("ok"));
        // UUID invalido -> false fail-open, mai PortError.
        assert!(!store.is_superseded("non-uuid").await.expect("fail-open"));
    }

    #[sqlx::test]
    async fn heartbeat_real_aggiorna_updated_at(pool: PgPool) {
        create_table(&pool).await;
        let store = PgRunControlStore::new(pool.clone());
        let run_id = insert_run(&pool, false).await;
        store
            .heartbeat(&run_id.to_string(), ExecMode::Real)
            .await
            .expect("ok");
        let updated: Option<chrono::DateTime<chrono::Utc>> =
            sqlx::query_scalar("SELECT updated_at FROM agent_runs WHERE id = $1")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .expect("riga");
        assert!(updated.is_some(), "heartbeat Real deve valorizzare updated_at");
    }

    #[sqlx::test]
    async fn heartbeat_replay_e_no_op(pool: PgPool) {
        create_table(&pool).await;
        let store = PgRunControlStore::new(pool.clone());
        let run_id = insert_run(&pool, false).await;
        store
            .heartbeat(&run_id.to_string(), ExecMode::Replay)
            .await
            .expect("ok");
        let updated: Option<chrono::DateTime<chrono::Utc>> =
            sqlx::query_scalar("SELECT updated_at FROM agent_runs WHERE id = $1")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .expect("riga");
        assert!(updated.is_none(), "in Replay nessun heartbeat");
    }

    #[sqlx::test]
    async fn set_effective_model_real_scrive_replay_no(pool: PgPool) {
        create_table(&pool).await;
        let store = PgRunControlStore::new(pool.clone());
        let run_id = insert_run(&pool, false).await;
        // Replay: no-op.
        store
            .set_effective_model(&run_id.to_string(), "anthropic", "m1", ExecMode::Replay)
            .await
            .expect("ok");
        let pm: (Option<String>, Option<String>) =
            sqlx::query_as("SELECT provider, model FROM agent_runs WHERE id = $1")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .expect("riga");
        assert_eq!(pm, (None, None), "Replay non scrive provider/model");
        // Real: scrive.
        store
            .set_effective_model(&run_id.to_string(), "anthropic", "m1", ExecMode::Real)
            .await
            .expect("ok");
        let pm: (Option<String>, Option<String>) =
            sqlx::query_as("SELECT provider, model FROM agent_runs WHERE id = $1")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .expect("riga");
        assert_eq!(pm, (Some("anthropic".to_string()), Some("m1".to_string())));
    }
}
