//! Cablaggio "process-completion -> agent-resume".
//!
//! Quando un `agent_process` (comando/servizio lanciato in background, tracciato
//! in DB) termina ed e' associato a una sessione chat, l'agente del run deve
//! essere RISVEGLIATO per dare l'aggiornamento promesso (tipico: l'agente lancia
//! una build lunga, dice "Ti aggiorno appena termina" e chiude il turno). Prima
//! mancava il collegamento: nessuno richiamava l'agente al completamento.
//!
//! Meccanismo (regola L: riusa il punto unico di avvio agente `spawn_agent_run`,
//! stesso pattern di `service_observer_remediation`): un worker periodico trova i
//! processi terminati di recente con `session_id` e `resume_dispatched_at IS
//! NULL`, inietta un messaggio sintetico con l'esito (exit code + coda output) e
//! avvia un nuovo turno agentico. Idempotenza via `resume_dispatched_at`;
//! anti-loop via cap orario per sessione. Tutta la config e' DB-driven (regola
//! G): `agent.process_resume.*` (mig 0360).

use std::time::Duration;

use sqlx::{PgPool, Row};
use tokio::time::sleep;
use uuid::Uuid;

use crate::agent_types::SupervisorMode;
use crate::chat_messages::{insert_message, spawn_agent_run, SpawnAgentParams};
use crate::orchestrator::AutomationMode;
use crate::AppState;

/// Attesa iniziale: lascia stabilizzare l'avvio prima del primo round.
const STARTUP_DELAY_S: u64 = 30;

pub fn spawn_process_resume_worker(state: AppState) {
    tokio::spawn(async move {
        sleep(Duration::from_secs(STARTUP_DELAY_S)).await;
        loop {
            let poll = load_u64(&state.db, "agent.process_resume.poll_seconds", 10, 5).await;
            if is_enabled(&state.db).await {
                if let Err(e) = run_one_round(&state).await {
                    tracing::warn!("process_resume: round fallito: {e}");
                }
            }
            sleep(Duration::from_secs(poll)).await;
        }
    });
    tracing::info!(
        "process_resume worker: avviato (cablaggio process-completion -> agent-resume)"
    );
}

async fn is_enabled(db: &PgPool) -> bool {
    crate::settings::get_setting(db, "agent.process_resume.enabled")
        .await
        .ok()
        .flatten()
        .map(|v| !matches!(v.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off"))
        .unwrap_or(true)
}

async fn load_u64(db: &PgPool, key: &str, default: u64, min: u64) -> u64 {
    crate::settings::get_setting(db, key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
        .max(min)
}

async fn load_i64(db: &PgPool, key: &str, default: i64) -> i64 {
    crate::settings::get_setting(db, key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(default)
}

/// Coda dell'output (stdout + eventuale stderr) per il messaggio di risveglio.
fn output_tail(output: &str, error_output: &str, max_chars: usize) -> String {
    let mut combined = output.trim().to_string();
    let err = error_output.trim();
    if !err.is_empty() {
        if !combined.is_empty() {
            combined.push_str("\n--- stderr ---\n");
        }
        combined.push_str(err);
    }
    if combined.chars().count() > max_chars {
        let tail: String = combined
            .chars()
            .rev()
            .take(max_chars)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("...(troncato)\n{tail}")
    } else {
        combined
    }
}

async fn run_one_round(state: &AppState) -> Result<(), String> {
    let rows = sqlx::query(
        "SELECT id, project_id, session_id, label, status, exit_code, output, error_output \
           FROM agent_processes \
          WHERE status IN ('stopped', 'failed') \
            AND session_id IS NOT NULL \
            AND resume_dispatched_at IS NULL \
            AND stopped_at > NOW() - INTERVAL '1 hour' \
          ORDER BY stopped_at ASC \
          LIMIT 5",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| e.to_string())?;

    let cap = load_i64(&state.db, "agent.process_resume.max_per_session_hour", 12).await;
    let tail_chars =
        load_u64(&state.db, "agent.process_resume.output_tail_chars", 2000, 200).await as usize;

    for row in rows {
        let id: Uuid = row.get("id");
        let project_id: Uuid = row.get("project_id");
        let session_id: Uuid = row.get("session_id");
        let label: String = row.try_get("label").unwrap_or_default();
        let status: String = row.try_get("status").unwrap_or_default();
        let exit_code: Option<i32> = row.try_get("exit_code").unwrap_or(None);
        let output: String = row.try_get("output").unwrap_or_default();
        let error_output: String = row.try_get("error_output").unwrap_or_default();

        // Marca SUBITO come dispatchato (idempotenza): meglio perdere un resume
        // che ripeterlo. Se rows_affected == 0 un altro ciclo l'ha gia' preso.
        let marked = sqlx::query(
            "UPDATE agent_processes SET resume_dispatched_at = NOW() \
              WHERE id = $1 AND resume_dispatched_at IS NULL",
        )
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(|e| e.to_string())?;
        if marked.rows_affected() == 0 {
            continue;
        }

        // Anti-loop: cap risvegli per sessione nell'ultima ora.
        let recent: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_processes \
              WHERE session_id = $1 AND resume_dispatched_at > NOW() - INTERVAL '1 hour'",
        )
        .bind(session_id)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
        if recent > cap {
            tracing::warn!(
                "process_resume: cap risvegli/ora ({}) per sessione {}, skip processo {}",
                cap,
                session_id,
                id
            );
            continue;
        }

        let owner: Option<Uuid> =
            sqlx::query_scalar("SELECT owner_user_id FROM projects WHERE id = $1")
                .bind(project_id)
                .fetch_optional(&state.db)
                .await
                .ok()
                .flatten();
        let owner = match owner {
            Some(o) => o,
            None => continue,
        };

        let tail = output_tail(&output, &error_output, tail_chars);
        let ok = status == "stopped" && exit_code.unwrap_or(0) == 0;
        let content = if ok {
            format!(
                "Il comando in background \"{label}\" e' terminato con SUCCESSO (exit_code={}).\n\n\
                 Output finale:\n```\n{tail}\n```\n\n\
                 Aggiorna l'utente sull'esito e prosegui con il prossimo passo, se previsto.",
                exit_code.unwrap_or(0)
            )
        } else {
            format!(
                "Il comando in background \"{label}\" e' FALLITO (exit_code={}).\n\n\
                 Output finale:\n```\n{tail}\n```\n\n\
                 Analizza l'errore nell'output e proponi (o applica) la correzione.",
                exit_code.unwrap_or(-1)
            )
        };
        let meta = serde_json::json!({
            "kind": "process_completion",
            "synthetic": true,
            "process_id": id.to_string(),
            "process_label": label,
            "exit_code": exit_code,
            "source": "process_resume",
        });

        let user_message_id =
            match insert_message(&state.db, session_id, project_id, "user", &content, meta, None)
                .await
            {
                Ok(mid) => mid,
                Err(e) => {
                    tracing::warn!("process_resume: insert messaggio sintetico fallito: {e:?}");
                    continue;
                }
            };

        let system_context = crate::prompt_templates::get_template_or_default(
            &state.db,
            &state.template_cache,
            "system.nexus_base",
        )
        .await;

        let params = SpawnAgentParams {
            user_id: owner,
            session_id,
            project_id,
            user_message_id,
            content,
            automation_mode: AutomationMode::Confirm,
            supervisor_mode: SupervisorMode::None,
            profile_prompt_block: String::new(),
            system_context,
            provider_override: None,
            model_override: None,
            profile_provider: None,
            profile_model: None,
            attachments: Vec::new(),
            user_role: "system".to_string(),
            nexus_agent_type_hint: None,
        };

        match spawn_agent_run(state, params).await {
            Some(r) => tracing::info!(
                "process_resume: agente risvegliato per processo {} (label='{}', run={})",
                id,
                label,
                r.run_id
            ),
            None => tracing::warn!(
                "process_resume: spawn_agent_run non ha prodotto un run per processo {}",
                id
            ),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_breve_invariato() {
        assert_eq!(output_tail("ciao", "", 2000), "ciao");
    }

    #[test]
    fn tail_combina_stderr() {
        let t = output_tail("out", "err", 2000);
        assert!(t.contains("out"));
        assert!(t.contains("--- stderr ---"));
        assert!(t.contains("err"));
    }

    #[test]
    fn tail_tronca_mantiene_la_coda() {
        let big = "a".repeat(5000);
        let t = output_tail(&big, "", 1000);
        assert!(t.starts_with("...(troncato)"));
        // Conserva al piu' max_chars di contenuto utile (+ il prefisso).
        assert!(t.chars().count() <= 1000 + 20);
    }
}
