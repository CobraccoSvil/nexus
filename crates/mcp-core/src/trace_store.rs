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
pub async fn get_session_traces(
    run_pool: &PgPool,
    session_id: Uuid,
    user_id: Uuid,
) -> Result<std::collections::HashMap<String, Vec<Value>>, sqlx::Error> {
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
        let payload: Value = row
            .try_get::<Option<Value>, _>("payload")
            .ok()
            .flatten()
            .unwrap_or_else(|| json!({}));
        runs.entry(run_id.to_string()).or_default().push(payload);
    }
    Ok(runs)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn create_tables(pool: &PgPool) {
        sqlx::query(
            "CREATE TABLE agent_runs ( \
                 id UUID PRIMARY KEY, \
                 session_id UUID NOT NULL, \
                 user_id UUID NOT NULL, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW() \
             )",
        )
        .execute(pool)
        .await
        .expect("create agent_runs");
        sqlx::query(
            "CREATE TABLE nexus_agent_traces ( \
                 id BIGSERIAL PRIMARY KEY, \
                 session_id UUID NOT NULL, \
                 run_id UUID NOT NULL, \
                 seq INTEGER NOT NULL DEFAULT 0, \
                 payload JSONB NOT NULL DEFAULT '{}'::jsonb, \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW() \
             )",
        )
        .execute(pool)
        .await
        .expect("create nexus_agent_traces");
    }

    #[sqlx::test]
    async fn persist_e_get_raggruppa_per_run(pool: PgPool) {
        create_tables(&pool).await;
        let session_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        sqlx::query("INSERT INTO agent_runs (id, session_id, user_id) VALUES ($1, $2, $3)")
            .bind(run_id)
            .bind(session_id)
            .bind(user_id)
            .execute(&pool)
            .await
            .expect("insert run");

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

    #[sqlx::test]
    async fn get_isola_per_utente(pool: PgPool) {
        create_tables(&pool).await;
        let session_id = Uuid::new_v4();
        let owner = Uuid::new_v4();
        let other = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        sqlx::query("INSERT INTO agent_runs (id, session_id, user_id) VALUES ($1, $2, $3)")
            .bind(run_id)
            .bind(session_id)
            .bind(owner)
            .execute(&pool)
            .await
            .expect("insert run");
        persist_trace(&pool, session_id, run_id, 0, &json!({"x": 1})).await;

        // L'altro utente non vede le tracce della sessione altrui.
        let runs = get_session_traces(&pool, session_id, other)
            .await
            .expect("get ok");
        assert!(runs.is_empty(), "nessun leak cross-utente");
    }
}
