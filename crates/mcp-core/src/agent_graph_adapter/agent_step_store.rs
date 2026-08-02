//! Adapter del trait [`nexus_agent_graph::runtime::ports::AgentStepStore`].
//!
//! Implementa `AgentStepStore::persist_step` con una INSERT su `agent_steps` via
//! `sqlx`. `step_index` deterministico = `iteration * 1000 + idx`. Idempotenza sui
//! retry + guard `untracked_run` in un'unica INSERT...SELECT:
//! `WHERE EXISTS (run in agent_runs)` (no FK orfane) `AND NOT EXISTS (step gia'
//! presente per run_id+step_index)` (equivale a `ON CONFLICT DO NOTHING`, ma
//! `agent_steps` ha solo un INDEX non-UNIQUE su `(run_id, step_index)` — mig 0009 —
//! quindi non esiste un constraint su cui fare `ON CONFLICT`). Best-effort: errore
//! DB loggato, `Ok(())`.
//!
//! Questo modulo NON interpreta: scrive in colonna i campi di
//! [`PersistedStep`]. Fino al 02/08/2026 li ri-derivava da un JSON opaco con
//! chiavi diverse da quelle che il produttore scriveva (`get("name")` contro
//! `"tool_name"`) e sovrascriveva lo `status` con un letterale `"completed"`:
//! ogni passo di ogni run nasceva anonimo e riuscito, fallimenti compresi. Il
//! rimedio non e' stato correggere le chiavi ma togliere le chiavi di mezzo —
//! vedi la nota su [`PersistedStep`].

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use nexus_agent_graph::runtime::ports::{AgentStepStore, PersistedStep, PortError};

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
    /// testa al modulo. Best-effort.
    async fn persist_step(
        &self,
        run_id: &str,
        iteration: i64,
        idx: i64,
        step: PersistedStep,
    ) -> Result<(), PortError> {
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
        let step_index: i32 = (iteration * nexus_agent_graph::runtime::ports::STEP_INDEX_STRIDE
            + idx)
            .clamp(0, i32::MAX as i64) as i32;
        // I campi del passo vanno in colonna COSI' COME SONO: nessuno va derivato
        // di nuovo qui (il produttore li ha gia' stabiliti) ne' sostituito da un
        // letterale. In particolare `status` viene da `outcome`, che il produttore
        // ha derivato dal flag strutturato `is_error` del tool_result (regola M).
        let PersistedStep {
            tool_name,
            tool_input,
            tool_result,
            status,
        } = step;
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
        .bind(status.as_str())
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
                self.warn_if_untracked(run_uuid, step_index, &tool_name)
                    .await;
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
    use nexus_agent_graph::runtime::ports::StepStatus;
    use serde_json::{json, Value};

    /// Run reale su cui appendere gli step: `agent_steps.run_id` e' vincolato da
    /// una FK verso `agent_runs(id)`. Le tabelle le porta il migrator del set
    /// `db/migrations/project`.
    async fn insert_run(pool: &PgPool) -> Uuid {
        crate::test_support::seed_agent_run(pool).await
    }

    /// Passo di prova. NON e' un input "fabbricato" nel senso della regola O: e'
    /// un tipo, non un JSON con chiavi da indovinare, quindi non puo' divergere
    /// in silenzio da cio' che il produttore costruisce — un rinominamento lo
    /// ferma il compilatore. Che il PRODUTTORE popoli questi campi con i valori
    /// giusti lo verifica il test gemello in `nexus-agent-graph`
    /// (`tool_dispatch::tests::persistenza_*`), che attraversa il nodo reale.
    fn passo(tool_name: &str, tool_input: Value, status: StepStatus) -> PersistedStep {
        PersistedStep {
            tool_name: tool_name.to_string(),
            tool_input,
            tool_result: Some("esito".to_string()),
            status,
        }
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn real_inserisce_con_step_index_deterministico(pool: PgPool) {
        let run_id = insert_run(&pool).await;
        let store = PgAgentStepStore::new(pool.clone());
        // iteration=3, idx=2 -> step_index = 3*1000+2 = 3002.
        store
            .persist_step(
                &run_id.to_string(),
                3,
                2,
                passo("edit_file", json!({"path": "x"}), StepStatus::Completed),
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
        assert_eq!(row.2.as_deref(), Some("esito"));
    }

    /// Il nome del tool e l'input finiscono nelle COLONNE omonime, e l'input ci
    /// finisce PIATTO: e' la forma che i consumatori interrogano
    /// (`criteria_runner`: `tool_input->>'path'`; `session_worklog`:
    /// `.get("command")`). L'impl scriveva invece l'intero involucro, quindi
    /// `tool_input->>'path'` era NULL su ogni riga.
    ///
    /// PROVA DI MUTAZIONE: rimettendo `.get("name")`/`.get("input")` al posto dei
    /// campi, `tool_name` torna `""` e `tool_input` torna l'involucro — entrambe
    /// le asserzioni rosseggiano col valore reale del difetto.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn nome_e_input_finiscono_nelle_colonne_in_forma_piatta(pool: PgPool) {
        let run_id = insert_run(&pool).await;
        let store = PgAgentStepStore::new(pool.clone());
        store
            .persist_step(
                &run_id.to_string(),
                0,
                0,
                passo(
                    "write_file",
                    json!({"path": "src/main.rs"}),
                    StepStatus::Completed,
                ),
            )
            .await
            .expect("ok");
        let (tool_name, path): (String, Option<String>) = sqlx::query_as(
            "SELECT tool_name, tool_input->>'path' FROM agent_steps WHERE run_id = $1",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .expect("riga");
        assert_eq!(
            tool_name, "write_file",
            "il nome del tool sta nella colonna tool_name, non annidato altrove"
        );
        assert_eq!(
            path.as_deref(),
            Some("src/main.rs"),
            "tool_input e' l'input del tool, non un involucro che lo contiene"
        );
    }

    /// Un passo FALLITO resta fallito in colonna. E' il difetto misurato il
    /// 02/08/2026: `.bind("completed")` LETTERALE scartava l'esito, e 536 passi
    /// falliti su 8860 (DB bacheca-attivita) risultavano riusciti a tutti i
    /// consumatori a valle.
    ///
    /// PROVA DI MUTAZIONE: rimettendo `.bind("completed")` al posto di
    /// `.bind(outcome.as_str())` questo test rosseggia con `"completed"` — cioe'
    /// esattamente il valore che il difetto produceva.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn un_passo_fallito_resta_fallito_in_colonna(pool: PgPool) {
        let run_id = insert_run(&pool).await;
        let store = PgAgentStepStore::new(pool.clone());
        for (idx, outcome) in [(0i64, StepStatus::Completed), (1, StepStatus::Failed)] {
            store
                .persist_step(
                    &run_id.to_string(),
                    0,
                    idx,
                    passo("run_command", json!({"command": "cargo test"}), outcome),
                )
                .await
                .expect("ok");
        }
        let stati: Vec<String> = sqlx::query_scalar(
            "SELECT status FROM agent_steps WHERE run_id = $1 ORDER BY step_index ASC",
        )
        .bind(run_id)
        .fetch_all(&pool)
        .await
        .expect("stati");
        assert_eq!(
            stati,
            vec!["completed".to_string(), "failed".to_string()],
            "lo status in colonna e' l'esito del passo, non un letterale"
        );
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn idempotente_sui_retry(pool: PgPool) {
        let run_id = insert_run(&pool).await;
        let store = PgAgentStepStore::new(pool.clone());
        for _ in 0..3 {
            store
                .persist_step(
                    &run_id.to_string(),
                    1,
                    0,
                    passo("read_file", json!({"path": "a.rs"}), StepStatus::Completed),
                )
                .await
                .expect("ok");
        }
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_steps WHERE run_id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(
            count, 1,
            "3 retry sullo stesso (run, step_index) -> 1 sola riga"
        );
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn untracked_run_non_inserisce(pool: PgPool) {
        // run_id NON presente in agent_runs: il guard EXISTS impedisce la FK orfana.
        let store = PgAgentStepStore::new(pool.clone());
        store
            .persist_step(
                &Uuid::new_v4().to_string(),
                0,
                0,
                passo("read_file", json!({}), StepStatus::Completed),
            )
            .await
            .expect("best-effort Ok");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_steps")
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 0, "run non tracciato -> nessuno step");
    }
}
