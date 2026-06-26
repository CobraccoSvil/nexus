//! Adapter del trait [`nexus_agent_graph::runtime::ports::MetaStepStore`].
//!
//! IMPLEMENTERA' (FASE 2) `MetaStepStore::persist_meta_step` con una INSERT su
//! `agent_meta_steps` via `sqlx` (plan/routing/clarify/fallback/reflection
//! persistiti per la cronologia, distinti dal canale live SSE
//! [`super::event_sink`]). Gata `Real` (no-op in `ExecMode::Replay`). Best-effort:
//! errore DB loggato, `Ok(())`. E' un trait SEPARATO da `EventSink` (persistenza
//! async/fallibile/gata vs canale live sincrono/infallibile, vedi doc del trait).

use async_trait::async_trait;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use nexus_agent_graph::runtime::ports::{ExecMode, MetaStepStore, PortError};

/// Adapter [`MetaStepStore`] -> `nexus_agent_meta_steps` via `sqlx`.
///
/// NOTA: la tabella DB e' `nexus_agent_meta_steps` (mig 0168), non `agent_meta_steps`.
/// La persistenza e' la copia per audit/timeline; il canale LIVE resta l'SSE
/// ([`super::event_sink`]).
pub struct PgMetaStepStore {
    /// Pool Postgres su cui gira la INSERT dei meta-step.
    db: PgPool,
    /// Run a cui i meta-step persistiti appartengono (colonna `run_id`, fissata alla
    /// costruzione: il `meta_step` JSON del trait non porta il `run_id`).
    run_id: Uuid,
}

impl PgMetaStepStore {
    /// Costruisce lo store sul pool Postgres condiviso per un dato run.
    pub fn new(db: PgPool, run_id: Uuid) -> Self {
        Self { db, run_id }
    }
}

#[async_trait]
impl MetaStepStore for PgMetaStepStore {
    /// INSERT best-effort su `nexus_agent_meta_steps`. Il `meta_step` JSON e'
    /// `{kind, title, payload, correlation_id?}` (forma del canale SSE); i campi
    /// mancanti degradano ai default di colonna (`title=''`, `payload='{}'`).
    ///
    /// Gate shadow (regola L): no-op in [`ExecMode::Replay`]. Best-effort: un
    /// `kind` vuoto o un errore DB sono loggati e ritornano `Ok(())` (parita' col
    /// best-effort psycopg2 di `brain/agents/meta_steps.py`).
    async fn persist_meta_step(&self, meta_step: Value, mode: ExecMode) -> Result<(), PortError> {
        if mode != ExecMode::Real {
            return Ok(());
        }
        let kind = meta_step
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if kind.is_empty() {
            tracing::warn!(
                run_id = %self.run_id,
                "meta_step_store: meta_step senza kind, INSERT saltata"
            );
            return Ok(());
        }
        let title = meta_step
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let payload = meta_step
            .get("payload")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let correlation_id = meta_step
            .get("correlation_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let res = sqlx::query(
            "INSERT INTO nexus_agent_meta_steps \
             (run_id, kind, title, payload, correlation_id) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(self.run_id)
        .bind(&kind)
        .bind(&title)
        .bind(&payload)
        .bind(correlation_id.as_deref())
        .execute(&self.db)
        .await;
        if let Err(e) = res {
            tracing::warn!(
                run_id = %self.run_id,
                kind = %kind,
                error = %e,
                "meta_step_store: INSERT nexus_agent_meta_steps fallita (best-effort)"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn create_table(pool: &PgPool) {
        sqlx::query(
            "CREATE TABLE nexus_agent_meta_steps ( \
                 id BIGSERIAL PRIMARY KEY, \
                 run_id UUID NOT NULL, \
                 kind TEXT NOT NULL, \
                 title TEXT NOT NULL DEFAULT '', \
                 payload JSONB NOT NULL DEFAULT '{}'::jsonb, \
                 correlation_id TEXT, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW() \
             )",
        )
        .execute(pool)
        .await
        .expect("create table meta_steps");
    }

    #[sqlx::test]
    async fn real_inserisce_con_kind(pool: PgPool) {
        create_table(&pool).await;
        let run_id = Uuid::new_v4();
        let store = PgMetaStepStore::new(pool.clone(), run_id);
        store
            .persist_meta_step(
                json!({"kind": "plan", "title": "Piano", "payload": {"n": 3}}),
                ExecMode::Real,
            )
            .await
            .expect("ok");
        let row: (String, String) = sqlx::query_as(
            "SELECT kind, title FROM nexus_agent_meta_steps WHERE run_id = $1",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .expect("riga");
        assert_eq!(row.0, "plan");
        assert_eq!(row.1, "Piano");
    }

    #[sqlx::test]
    async fn replay_e_no_op(pool: PgPool) {
        create_table(&pool).await;
        let store = PgMetaStepStore::new(pool.clone(), Uuid::new_v4());
        store
            .persist_meta_step(json!({"kind": "routing"}), ExecMode::Replay)
            .await
            .expect("ok");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nexus_agent_meta_steps")
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 0, "in Replay nessuna scrittura");
    }

    #[sqlx::test]
    async fn kind_vuoto_non_inserisce(pool: PgPool) {
        create_table(&pool).await;
        let store = PgMetaStepStore::new(pool.clone(), Uuid::new_v4());
        store
            .persist_meta_step(json!({"title": "senza kind"}), ExecMode::Real)
            .await
            .expect("best-effort Ok");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nexus_agent_meta_steps")
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 0);
    }
}
