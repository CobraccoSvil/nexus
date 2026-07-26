//! Persistenza delle tracce gateway LLM (`AITraceEvent`) su `nexus_agent_traces`
//! (mig 0485). Speculare a [`crate::agent_graph_adapter::meta_step_store`] per i
//! meta-step: il canale LIVE resta l'SSE (`agent_trace`), questa tabella e' la
//! copia persistente che permette di ricostruire il trace panel dopo un reload.
//!
//! Prima del FIX D7 le tracce vivevano SOLO in `sessionStorage` del browser
//! (volatile, per-dispositivo): dopo un refresh in un altro tab/dispositivo o
//! dopo aver pulito lo storage il pannello tracce divergeva dal rendering live.
//!
//! Punto unico (regola L): l'INSERT vive qui; i call site (oggi
//! `agent_turn_setup`) delegano a [`persist_trace`], niente query SQL
//! duplicate. Il getter [`get_session_traces`] raggruppa per `run_id`, stessa
//! shape di `chat_agent::get_session_meta_steps` (`{ runs: { runId: [...] } }`).

use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// INSERT best-effort di una traccia gateway su `nexus_agent_traces`.
///
/// `payload` e' l'`AITraceEvent` gia' serializzato (camelCase, stessa forma
/// dell'evento SSE `agent_trace`). Best-effort come il fratello meta-step: un
/// errore DB e' loggato e ignorato (gli eventi SSE arrivano comunque, il run non
/// si interrompe per una persistenza di telemetria). `seq` e' l'indice
/// progressivo della traccia nel run (di norma l'iteration dell'AITraceEvent).
///
/// `run_pool` DEVE essere il pool del DB dove vive il run (progetto a flag
/// separazione ON): la risoluzione e' responsabilita' del chiamante, UNA volta
/// (regola L). Risolvere QUI con il pool ricevuto era una doppia risoluzione:
/// event_sink/chat_agent passano gia' il pool progetto e la ri-risoluzione
/// interrogava settings/directory sul DB progetto, dove non esistono, con
/// rischio di avvelenare per 30s il flag globale di separazione.
pub async fn persist_trace(
    run_pool: &PgPool,
    session_id: Uuid,
    run_id: Uuid,
    seq: i32,
    payload: &Value,
) {
    let res = sqlx::query(
        "INSERT INTO nexus_agent_traces (session_id, run_id, seq, payload) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(session_id)
    .bind(run_id)
    .bind(seq)
    .bind(payload)
    .execute(run_pool)
    .await;
    if let Err(e) = res {
        tracing::warn!(
            run_id = %run_id,
            error = %e,
            "trace_store: INSERT nexus_agent_traces fallita (best-effort)"
        );
    }
}

/// Legge tutte le tracce dei run di una sessione, raggruppate per `run_id`.
///
/// Stessa shape e stessi vincoli di `get_session_meta_steps`: ownership via
/// filtro sui run della sessione di proprieta' dell'utente (nessun leak
/// cross-utente), limite agli ultimi 30 run includendo SEMPRE i piu' recenti
/// (il caso d'uso del refresh), ordine cronologico (`seq` poi `created_at`) per
/// la ricostruzione fedele del pannello. Ritorna `{ "<run_id>": [payload...] }`.
/// `run_pool` DEVE essere il pool del DB dove vivono i run della sessione
/// (progetto a flag separazione ON), risolto dal chiamante — stessa convenzione
/// di [`persist_trace`] (regola L: la risoluzione vive su UN solo lato).
///
/// ## `parentRunId`: la parentela viaggia col dato che deve essere attribuito
///
/// Le tracce di un sub-agente sono persistite sotto il `run_id` PROPRIO del
/// figlio, non sotto quello del padre. Chi somma i token o compone la
/// ripartizione per provider di un run deve quindi sapere quali run lo
/// compongono, e finora il frontend lo deduceva dai meta-step di NARRAZIONE: un
/// canale di presentazione, che il review panel non emette affatto. Risultato
/// misurato: 4 cicli di review su openrouter spariti dalla barra dei costi.
///
/// Qui ogni traccia di un sub-run viene annotata col campo `parentRunId` letto
/// dal PUNTO UNICO della parentela ([`crate::run_lineage`], che la prende dal
/// DB). Il campo NON e' persistito in `nexus_agent_traces`: e' una join in
/// lettura, quindi non puo' divergere dalla fonte. Se la lettura della parentela
/// fallisce le tracce escono comunque (senza annotazione): la telemetria degrada,
/// non sparisce.
pub async fn get_session_traces(
    run_pool: &PgPool,
    session_id: Uuid,
    user_id: Uuid,
) -> Result<std::collections::HashMap<String, Vec<Value>>, sqlx::Error> {
    let parent_by_child = crate::run_lineage::parent_run_by_child(run_pool, session_id, user_id)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(
                session_id = %session_id,
                error = %e,
                "trace_store: parentela sub-run non leggibile, tracce senza parentRunId"
            );
            std::collections::HashMap::new()
        });
    let rows = sqlx::query(
        "SELECT t.run_id, t.payload \
         FROM nexus_agent_traces t \
         WHERE t.run_id IN ( \
             SELECT id FROM agent_runs \
             WHERE session_id = $1 AND user_id = $2 \
             ORDER BY created_at DESC \
             LIMIT 30 \
         ) \
         ORDER BY t.seq ASC, t.created_at ASC",
    )
    .bind(session_id)
    .bind(user_id)
    .fetch_all(run_pool)
    .await?;

    let mut runs: std::collections::HashMap<String, Vec<Value>> = std::collections::HashMap::new();
    for row in rows {
        let run_id: Uuid = match row.try_get("run_id") {
            Ok(v) => v,
            Err(_) => continue,
        };
        let mut payload: Value = row
            .try_get::<Option<Value>, _>("payload")
            .ok()
            .flatten()
            .unwrap_or_else(|| json!({}));
        if let Some(parent) = parent_by_child.get(&run_id) {
            // camelCase come il resto dell'AITraceEvent (stessa forma dell'evento
            // SSE): il tipo TypeScript legge `parentRunId`.
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("parentRunId".to_string(), json!(parent.to_string()));
            }
        }
        runs.entry(run_id.to_string()).or_default().push(payload);
    }
    Ok(runs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sessione + run dell'utente indicato. Le tabelle le porta il migrator del
    /// set project: qui si semina solo la riga, coi NOT NULL e la FK
    /// `agent_runs.session_id -> chat_sessions(id)` che lo schema reale impone.
    async fn seed_run(pool: &PgPool, user_id: Uuid) -> (Uuid, Uuid) {
        let project_id = Uuid::new_v4();
        let session_id = crate::test_support::seed_chat_session(pool, project_id).await;
        let run_id = crate::test_support::insert_agent_run_as(
            pool, session_id, project_id, user_id, "running",
        )
        .await;
        (session_id, run_id)
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn persist_e_get_raggruppa_per_run(pool: PgPool) {
        let user_id = Uuid::new_v4();
        let (session_id, run_id) = seed_run(&pool, user_id).await;

        persist_trace(
            &pool,
            session_id,
            run_id,
            0,
            &json!({"runId": run_id.to_string(), "provider": "mistral", "iteration": 0}),
        )
        .await;
        persist_trace(
            &pool,
            session_id,
            run_id,
            1,
            &json!({"runId": run_id.to_string(), "provider": "google", "iteration": 1}),
        )
        .await;

        let runs = get_session_traces(&pool, session_id, user_id)
            .await
            .expect("get ok");
        let traces = runs.get(&run_id.to_string()).expect("run presente");
        assert_eq!(traces.len(), 2, "due tracce per il run");
        // Ordine per seq: la prima e' iteration 0 (mistral), la seconda google.
        assert_eq!(traces[0]["provider"], json!("mistral"));
        assert_eq!(traces[1]["provider"], json!("google"));
    }

    /// REGRESSIONE (2026-07-26): la barra costo-per-provider di un run ometteva
    /// il provider dei revisori. Le tracce del sub-run esistono, sotto il run_id
    /// del figlio; mancava sul wire il fatto che quel run appartiene al padre,
    /// perche' il frontend lo deduceva dai meta-step di NARRAZIONE (che il review
    /// panel non emette). Qui si verifica il produttore reale del wire: la
    /// traccia del figlio esce annotata col `parentRunId` del run che lo ha
    /// convocato, quella del padre no.
    ///
    /// Mutazione: togliendo l'annotazione in `get_session_traces`, il campo e'
    /// assente e la prima assert fallisce con `null`.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn la_traccia_del_subrun_dichiara_il_run_padre(pool: PgPool) {
        let user_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let session_id = crate::test_support::seed_chat_session(&pool, project_id).await;
        let padre = crate::test_support::insert_agent_run_as(
            &pool, session_id, project_id, user_id, "completed",
        )
        .await;
        let revisore = crate::test_support::seed_subagent_run(
            &pool,
            session_id,
            project_id,
            user_id,
            padre,
            Some(padre),
            "review",
        )
        .await;

        persist_trace(
            &pool,
            session_id,
            padre,
            0,
            &json!({"runId": padre.to_string(), "provider": "deepseek"}),
        )
        .await;
        persist_trace(
            &pool,
            session_id,
            revisore,
            0,
            &json!({"runId": revisore.to_string(), "provider": "openrouter"}),
        )
        .await;

        let runs = get_session_traces(&pool, session_id, user_id)
            .await
            .expect("get ok");
        let del_figlio = &runs.get(&revisore.to_string()).expect("sub-run presente")[0];
        assert_eq!(
            del_figlio["parentRunId"],
            json!(padre.to_string()),
            "la traccia del revisore dichiara il run che lo ha convocato"
        );
        let del_padre = &runs.get(&padre.to_string()).expect("run padre presente")[0];
        assert_eq!(
            del_padre.get("parentRunId"),
            None,
            "un run primario non ha un run padre"
        );
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn get_isola_per_utente(pool: PgPool) {
        let owner = Uuid::new_v4();
        let other = Uuid::new_v4();
        let (session_id, run_id) = seed_run(&pool, owner).await;
        persist_trace(&pool, session_id, run_id, 0, &json!({"x": 1})).await;

        // L'altro utente non vede le tracce della sessione altrui.
        let runs = get_session_traces(&pool, session_id, other)
            .await
            .expect("get ok");
        assert!(runs.is_empty(), "nessun leak cross-utente");
    }
}
