//! Adapter del trait [`nexus_agent_graph::runtime::ports::MetaStepStore`].
//!
//! IMPLEMENTERA' (FASE 2) `MetaStepStore::persist_meta_step` con una INSERT su
//! `agent_meta_steps` via `sqlx` (plan/routing/clarify/fallback/reflection
//! persistiti per la cronologia, distinti dal canale live SSE
//! [`super::event_sink`]). Best-effort: errore DB loggato, `Ok(())`. E' un trait
//! SEPARATO da `EventSink` (persistenza async/fallibile vs canale live
//! sincrono/infallibile, vedi doc del trait).

use async_trait::async_trait;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use nexus_agent_graph::runtime::ports::{MetaStepStore, PortError};

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

    /// Il PIANO non e' append-only come gli altri kind: e' lo STATO del run e ha
    /// un solo posto dove stare (indice unico parziale, mig project 0018). Qui
    /// l'INSERT cieca era il secondo produttore del difetto misurato il
    /// 10/08/2026 — due righe `plan` identiche a 2,3 ms l'una dall'altra, il
    /// piano reso due volte in chat. Si delega al punto unico
    /// [`nexus_agent_tools::meta_piano`], che fonde il payload col precedente
    /// invece di affiancarlo, e che compone titolo e `n` dai campi.
    async fn scrivi_piano(&self, payload: &Value) {
        if let Err(e) = nexus_agent_tools::meta_piano::scrivi(&self.db, self.run_id, payload).await {
            tracing::warn!(
                run_id = %self.run_id,
                error = %e,
                "meta_step_store: UPSERT del piano fallito (best-effort)"
            );
        }
    }

    /// Riga nuova in coda alla timeline: la forma di TUTTI gli altri kind, che
    /// sono cronologia (`subagent_progress`, `routing`, `escalation`...).
    async fn accoda_riga(
        &self,
        kind: &str,
        title: &str,
        payload: &Value,
        correlation_id: Option<&str>,
    ) {
        let res = sqlx::query(
            "INSERT INTO nexus_agent_meta_steps \
             (run_id, kind, title, payload, correlation_id) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(self.run_id)
        .bind(kind)
        .bind(title)
        .bind(payload)
        .bind(correlation_id)
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
    }
}

#[async_trait]
impl MetaStepStore for PgMetaStepStore {
    /// INSERT best-effort su `nexus_agent_meta_steps`. Il `meta_step` JSON e'
    /// `{kind, title, payload, correlation_id?}` (forma del canale SSE); i campi
    /// mancanti degradano ai default di colonna (`title=''`, `payload='{}'`).
    ///
    /// Best-effort: un `kind` vuoto o un errore DB sono loggati e ritornano
    /// `Ok(())` (parita' col best-effort psycopg2 di `brain/agents/meta_steps.py`).
    async fn persist_meta_step(&self, meta_step: Value) -> Result<(), PortError> {
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
        if kind == nexus_agent_tools::meta_piano::KIND_PIANO {
            self.scrivi_piano(&payload).await;
        } else {
            self.accoda_riga(&kind, &title, &payload, correlation_id.as_deref())
                .await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn real_inserisce_con_kind(pool: PgPool) {
        let run_id = Uuid::new_v4();
        let store = PgMetaStepStore::new(pool.clone(), run_id);
        store
            .persist_meta_step(json!({"kind": "routing", "title": "Routing", "payload": {"n": 3}}))
            .await
            .expect("ok");
        let row: (String, String) =
            sqlx::query_as("SELECT kind, title FROM nexus_agent_meta_steps WHERE run_id = $1")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .expect("riga");
        assert_eq!(row.0, "routing");
        assert_eq!(row.1, "Routing");
    }

    /// Il titolo del piano lo COMPONE il punto unico dai campi (regola Q): i due
    /// produttori ne dichiaravano due diversi ("Piano — N step" e "Piano creato
    /// — N step") per lo stesso identico piano.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn il_piano_prende_titolo_e_numero_dai_todo(pool: PgPool) {
        let run_id = Uuid::new_v4();
        let store = PgMetaStepStore::new(pool.clone(), run_id);
        store
            .persist_meta_step(json!({
                "kind": "plan",
                "title": "Piano creato — 2 step",
                "payload": {"todos": [{"id": "a"}, {"id": "b"}], "provider": "mistral"},
            }))
            .await
            .expect("ok");
        let (titolo, payload): (String, serde_json::Value) = sqlx::query_as(
            "SELECT title, payload FROM nexus_agent_meta_steps WHERE run_id = $1 AND kind = 'plan'",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .expect("riga");
        assert_eq!(titolo, "Piano — 2 step");
        assert_eq!(payload.get("n").and_then(serde_json::Value::as_u64), Some(2));
    }

    /// Il difetto del 10/08/2026: il planner (questo adapter) e il tool
    /// `nexus_todo_write` scrivevano DUE righe `plan` per lo stesso run, a 2,3 ms
    /// l'una dall'altra e con lo stesso array di todo — in chat il piano
    /// compariva due volte.
    ///
    /// Il test attraversa i DUE produttori reali: l'adapter qui sopra e il punto
    /// unico chiamato da `todos::persisti_meta_piano`, che rilegge i todo dal DB.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn i_due_produttori_scrivono_una_riga_sola_e_nessuno_perde_i_propri_campi(pool: PgPool) {
        let run_id = crate::test_support::seed_agent_run(&pool).await;
        crate::test_support::seed_todo(&pool, run_id, 1, "completed").await;
        crate::test_support::seed_todo(&pool, run_id, 2, "pending").await;

        // Percorso TOOL: stato dei todo, riletto dal DB.
        nexus_agent_tools::meta_piano::scrivi_dai_todo(&pool, run_id).await;
        // Percorso PLANNER: provenienza del piano, via la porta MetaStepStore.
        PgMetaStepStore::new(pool.clone(), run_id)
            .persist_meta_step(json!({
                "kind": "plan",
                "title": "Piano creato — 2 step",
                "payload": {
                    "todos": [{"id": "a", "status": "pending"}, {"id": "b", "status": "pending"}],
                    "plan_id": run_id.to_string(),
                    "provider": "mistral",
                    "model": "mistral-small-latest",
                },
            }))
            .await
            .expect("best-effort Ok");

        let righe: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM nexus_agent_meta_steps WHERE run_id = $1 AND kind = 'plan'",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .expect("count");
        assert_eq!(righe, 1, "il piano di un run e' UNA riga, non una cronologia");

        let payload: serde_json::Value = sqlx::query_scalar(
            "SELECT payload FROM nexus_agent_meta_steps WHERE run_id = $1 AND kind = 'plan'",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .expect("payload");
        // Fusione: chi scrive vince sulle proprie chiavi, cio' che tace resta.
        assert_eq!(
            payload.get("provider").and_then(serde_json::Value::as_str),
            Some("mistral"),
            "la provenienza del planner non deve sparire"
        );
        assert_eq!(
            payload.get("n").and_then(serde_json::Value::as_u64),
            Some(2),
            "n resta derivato dai todo dell'ultimo scrittore"
        );
    }

    /// Il piano e' l'ECCEZIONE: ogni altro kind resta append-only (la narrazione
    /// di un sub-agente e' una cronologia, non uno stato).
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn gli_altri_kind_restano_append_only(pool: PgPool) {
        let run_id = Uuid::new_v4();
        let store = PgMetaStepStore::new(pool.clone(), run_id);
        for i in 0..2 {
            store
                .persist_meta_step(json!({
                    "kind": "subagent_progress",
                    "title": format!("passo {i}"),
                    "payload": {},
                }))
                .await
                .expect("ok");
        }
        let righe: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM nexus_agent_meta_steps WHERE run_id = $1")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .expect("count");
        assert_eq!(righe, 2);
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn kind_vuoto_non_inserisce(pool: PgPool) {
        let store = PgMetaStepStore::new(pool.clone(), Uuid::new_v4());
        store
            .persist_meta_step(json!({"title": "senza kind"}))
            .await
            .expect("best-effort Ok");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nexus_agent_meta_steps")
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 0);
    }
}
