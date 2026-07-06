//! Adapter del trait [`nexus_agent_graph::runtime::ports::AgentStepStore`].
//!
//! Implementa `AgentStepStore::persist_step` con una INSERT su `agent_steps` via
//! `sqlx`. `step_index` deterministico = `iteration * 1000 + idx`. Idempotenza sui
//! retry + guard `untracked_run` in un'unica INSERT...SELECT:
//! `WHERE EXISTS (run in agent_runs)` (no FK orfane) `AND NOT EXISTS (step gia'
//! presente per run_id+step_index)` (equivale a `ON CONFLICT DO NOTHING`, ma
//! `agent_steps` ha solo un INDEX non-UNIQUE su `(run_id, step_index)` — mig 0009 —
//! quindi non esiste un constraint su cui fare `ON CONFLICT`). Gata `Real` (no-op in
//! `ExecMode::Replay`, punto unico del gate shadow). Best-effort: errore DB loggato,
//! `Ok(())`.
//!
//! NB: questa e' anche la fonte Replay (F3): la riga porta `run_id`, `step_index`,
//! `tool_name`, `tool_input` (block), `tool_result` (result) e `status`. F3
//! rileggera' il `tool_result` del run primario da qui filtrando per `run_id` +
//! `step_index` (ordinamento globale stabile).

use async_trait::async_trait;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use nexus_agent_graph::runtime::ports::{AgentStepStore, ExecMode, PortError};

/// Adapter [`AgentStepStore`] -> `agent_steps` via `sqlx`.
pub struct PgAgentStepStore {
    /// Pool Postgres su cui gira la INSERT degli step.
    db: PgPool,
}

impl PgAgentStepStore {
    /// Costruisce lo store sul pool Postgres condiviso.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait]
impl AgentStepStore for PgAgentStepStore {
    /// Persiste UN blocco di un'iterazione su `agent_steps`. Vedi il mapping in
    /// testa al modulo. Gata `Real` (regola L); best-effort.
    async fn persist_step(
        &self,
        run_id: &str,
        iteration: i64,
        idx: i64,
        block: Value,
        result: Option<Value>,
        mode: ExecMode,
    ) -> Result<(), PortError> {
        if mode != ExecMode::Real {
            return Ok(());
        }
        let run_uuid = match Uuid::parse_str(run_id) {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!(
                    run_id = %run_id,
                    error = %e,
                    "agent_step_store: run_id non e' un UUID, INSERT saltata"
                );
                return Ok(());
            }
        };
        // step_index deterministico (i32 in DB, mig 0009). iteration/idx sono
        // limitati (iteration < ~1000, idx < 1000): clamp difensivo a i32::MAX.
        let step_index: i32 = (iteration * 1000 + idx).clamp(0, i32::MAX as i64) as i32;
        // tool_name / tool_result derivati dal block/result per riempire le colonne
        // NOT NULL della tabella. Il block e' il blocco grezzo dell'iterazione
        // (tool_use/testo); il nome del tool, se presente, popola tool_name.
        let tool_name = block
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let tool_input = block
            .get("input")
            .cloned()
            .unwrap_or_else(|| block.clone());
        let tool_result: Option<String> = result.map(|r| match r {
            Value::String(s) => s,
            other => other.to_string(),
        });
        // INSERT...SELECT con doppio guard: il run deve esistere (no FK orfana,
        // "untracked_run") e lo step non deve esistere gia' (idempotenza retry,
        // surrogato di ON CONFLICT DO NOTHING in assenza di constraint UNIQUE).
        let res = sqlx::query(
            "INSERT INTO agent_steps \
             (id, run_id, step_index, tool_name, tool_input, tool_result, status, created_at) \
             SELECT gen_random_uuid(), $1, $2, $3, $4, $5, $6, NOW() \
             WHERE EXISTS (SELECT 1 FROM agent_runs WHERE id = $1) \
               AND NOT EXISTS ( \
                   SELECT 1 FROM agent_steps WHERE run_id = $1 AND step_index = $2 \
               )",
        )
        .bind(run_uuid)
        .bind(step_index)
        .bind(&tool_name)
        .bind(&tool_input)
        .bind(tool_result.as_deref())
        .bind("completed")
        .execute(&self.db)
        .await;
        match res {
            Err(e) => tracing::warn!(
                run_id = %run_id,
                step_index,
                error = %e,
                "agent_step_store: INSERT agent_steps fallita (best-effort)"
            ),
            // 0 righe = guard scattato: o retry idempotente (step gia' presente,
            // benigno) o RUN NON TRACCIATO in agent_runs -> lo step e' PERSO.
            Ok(r) if r.rows_affected() == 0 => {
                self.warn_if_untracked(run_uuid, step_index, &tool_name).await;
            }
            Ok(_) => {}
        }
        Ok(())
    }
}

impl PgAgentStepStore {
    /// Distingue e logga il caso "run non tracciato" quando il guard EXISTS ha
    /// scartato uno step (regola M: mai buchi neri silenziosi — l'incidente
    /// sub-agenti 2026-07-06 e' rimasto invisibile proprio perche' gli step dei
    /// figli venivano scartati senza traccia). Solo diagnosi, mai errore.
    async fn warn_if_untracked(&self, run_uuid: Uuid, step_index: i32, tool_name: &str) {
        let tracked: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM agent_runs WHERE id = $1)")
                .bind(run_uuid)
                .fetch_one(&self.db)
                .await
                .unwrap_or(true);
        if !tracked {
            tracing::warn!(
                run_id = %run_uuid,
                step_index,
                tool = %tool_name,
                "agent_step_store: run NON tracciato in agent_runs, step SCARTATO \
                 (osservabilita' persa: creare la riga run prima degli step)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// agent_runs minimale (guard untracked_run) + agent_steps (mig 0009).
    async fn create_tables(pool: &PgPool) {
        sqlx::query("CREATE TABLE agent_runs (id UUID PRIMARY KEY DEFAULT gen_random_uuid())")
            .execute(pool)
            .await
            .expect("create agent_runs");
        sqlx::query(
            "CREATE TABLE agent_steps ( \
                 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), \
                 run_id UUID NOT NULL, \
                 step_index INT NOT NULL, \
                 tool_name TEXT NOT NULL, \
                 tool_input JSONB NOT NULL, \
                 tool_result TEXT, \
                 status TEXT NOT NULL DEFAULT 'running', \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW() \
             )",
        )
        .execute(pool)
        .await
        .expect("create agent_steps");
    }

    async fn insert_run(pool: &PgPool) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO agent_runs (id) VALUES ($1)")
            .bind(id)
            .execute(pool)
            .await
            .expect("insert run");
        id
    }

    #[sqlx::test]
    async fn real_inserisce_con_step_index_deterministico(pool: PgPool) {
        create_tables(&pool).await;
        let run_id = insert_run(&pool).await;
        let store = PgAgentStepStore::new(pool.clone());
        // iteration=3, idx=2 -> step_index = 3*1000+2 = 3002.
        store
            .persist_step(
                &run_id.to_string(),
                3,
                2,
                json!({"name": "edit_file", "input": {"p": "x"}}),
                Some(json!("done")),
                ExecMode::Real,
            )
            .await
            .expect("ok");
        let row: (i32, String, Option<String>) = sqlx::query_as(
            "SELECT step_index, tool_name, tool_result FROM agent_steps WHERE run_id = $1",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .expect("riga");
        assert_eq!(row.0, 3002, "step_index = iteration*1000 + idx");
        assert_eq!(row.1, "edit_file");
        assert_eq!(row.2.as_deref(), Some("done"));
    }

    #[sqlx::test]
    async fn idempotente_sui_retry(pool: PgPool) {
        create_tables(&pool).await;
        let run_id = insert_run(&pool).await;
        let store = PgAgentStepStore::new(pool.clone());
        let block = json!({"name": "read_file"});
        for _ in 0..3 {
            store
                .persist_step(&run_id.to_string(), 1, 0, block.clone(), None, ExecMode::Real)
                .await
                .expect("ok");
        }
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_steps WHERE run_id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 1, "3 retry sullo stesso (run, step_index) -> 1 sola riga");
    }

    #[sqlx::test]
    async fn untracked_run_non_inserisce(pool: PgPool) {
        create_tables(&pool).await;
        // run_id NON presente in agent_runs: il guard EXISTS impedisce la FK orfana.
        let store = PgAgentStepStore::new(pool.clone());
        store
            .persist_step(&Uuid::new_v4().to_string(), 0, 0, json!({}), None, ExecMode::Real)
            .await
            .expect("best-effort Ok");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_steps")
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 0, "run non tracciato -> nessuno step");
    }

    #[sqlx::test]
    async fn replay_e_no_op(pool: PgPool) {
        create_tables(&pool).await;
        let run_id = insert_run(&pool).await;
        let store = PgAgentStepStore::new(pool.clone());
        store
            .persist_step(&run_id.to_string(), 1, 0, json!({}), None, ExecMode::Replay)
            .await
            .expect("ok");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_steps")
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 0, "in Replay (shadow) nessuna scrittura");
    }
}
