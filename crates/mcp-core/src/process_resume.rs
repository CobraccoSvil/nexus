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

/// Annuncia in chat (una sola volta per sessione/ora) che il cap anti-loop dei
/// risvegli automatici e' scattato. Senza questo messaggio l'utente vede la chat
/// "ferma" senza capirne il motivo (il safety net opera silenzioso). Idempotente
/// via query: se l'ultima ora ha gia' un messaggio sintetico `kind='cap_reached'`
/// per quella sessione, no-op.
async fn announce_cap_reached_in_chat(
    db: &sqlx::PgPool,
    session_id: Uuid,
    project_id: Uuid,
    cap: i64,
    label: &str,
) {
    let already: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM chat_messages \
         WHERE session_id = $1 \
           AND created_at > NOW() - INTERVAL '1 hour' \
           AND metadata ->> 'kind' = 'cap_reached'",
    )
    .bind(session_id)
    .fetch_one(db)
    .await
    .unwrap_or(0);
    if already > 0 {
        return;
    }
    let content = format!(
        "Cap anti-loop raggiunto: il processo di sfondo \"{label}\" ha innescato \
         {cap} risvegli automatici nell'ultima ora. L'agente NON verra' piu' \
         risvegliato finche' la finestra oraria non si svuota. Cosa significa: \
         l'agente stava ricucendo lo stesso comando in un loop (es. servizio che \
         continua a terminare). Cosa fare: indagare la causa del fallimento \
         ripetuto (container in crash-loop, dipendenze non pronte, porta \
         occupata) prima di chiedere all'agente di riprovare."
    );
    let meta = serde_json::json!({
        "kind": "cap_reached",
        "synthetic": true,
        "session_id": session_id.to_string(),
        "label": label,
        "cap": cap,
        "source": "process_resume",
    });
    if let Err(e) =
        insert_message(db, session_id, project_id, "user", &content, meta, None).await
    {
        tracing::warn!("process_resume: cap_reached announce fallito: {e:?}");
    }
}

/// True se la riga di comando appartiene a un avvio docker compose. Cattura sia
/// `docker compose up` sia `docker-compose up` (vecchio plugin).
fn is_docker_compose_command(cmd: &str) -> bool {
    let l = cmd.to_lowercase();
    (l.contains("docker compose") || l.contains("docker-compose")) && l.contains(" up")
}

#[derive(Debug, Clone)]
struct DockerHealth {
    healthy: bool,
    summary: String,
}

/// Verita' post-mortem sui container DEL PROGETTO (filtrati per slug, regola E:
/// mai container globali / ideai-*). Esito `healthy=true` solo se TUTTI i
/// container del progetto sono in stato "running" (o "healthy" se hanno
/// healthcheck). Restart in corso / Exited / Restarting -> healthy=false con
/// summary leggibile da iniettare all'agente.
async fn inspect_docker_compose_health(db: &sqlx::PgPool, project_id: Uuid) -> Option<DockerHealth> {
    let slug: String =
        sqlx::query_scalar("SELECT lower(replace(replace(name, ' ', '-'), '_', '-')) FROM projects WHERE id = $1")
            .bind(project_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten()?;
    if slug.is_empty() {
        return None;
    }
    let out = tokio::process::Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            &format!("name={slug}-"),
            "--format",
            "{{.Names}}\t{{.Status}}",
        ])
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return None;
    }
    let mut healthy = true;
    for line in &lines {
        let lower = line.to_lowercase();
        // "Up X minutes" o "Up X (healthy)" = ok. Restarting / Exited / unhealthy = ko.
        let is_up = lower.contains("\tup ") || lower.contains("\tup(");
        let is_bad = lower.contains("restarting")
            || lower.contains("exited")
            || lower.contains("unhealthy")
            || lower.contains("dead")
            || lower.contains("created");
        if !is_up || is_bad {
            healthy = false;
        }
    }
    Some(DockerHealth {
        healthy,
        summary: lines.join("\n"),
    })
}

async fn run_one_round(state: &AppState) -> Result<(), String> {
    let rows = sqlx::query(
        "SELECT id, project_id, session_id, label, command, status, exit_code, output, error_output \
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
        let command: String = row.try_get("command").unwrap_or_default();
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
            // Visibilita' in UI (regola: lo stato interno si vede sempre):
            // un solo messaggio sintetico per sessione/ora con kind='cap_reached',
            // cosi' l'utente capisce che la chat e' in pausa e perche'. Idempotente:
            // se esiste gia' un messaggio cap_reached nell'ultima ora, non lo
            // duplico (anti-spam strutturale, niente set in memoria).
            announce_cap_reached_in_chat(&state.db, session_id, project_id, cap, &label).await;
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
        // Verita' del processo: exit_code da solo NON basta per docker compose
        // up: `docker compose up` puo' uscire con exit_code=0 anche se uno o
        // piu' container del progetto sono in stato Restarting/Exited. Senza
        // questo check post-mortem, l'agente riceveva "SUCCESSO", concludeva
        // che il servizio era attivo, e — vedendo che non rispondeva — lo
        // rilanciava: loop osservato finche' il cap risvegli non lo fermava.
        // Container-aware: se il comando e' un docker compose, controlliamo i
        // container del progetto e degradiamo l'esito a FALLITO se almeno uno
        // non e' realmente up.
        let docker_health = if is_docker_compose_command(&command) {
            inspect_docker_compose_health(&state.db, project_id).await
        } else {
            None
        };
        let ok = status == "stopped" && exit_code.unwrap_or(0) == 0
            && docker_health.as_ref().map(|h| h.healthy).unwrap_or(true);
        let docker_note = docker_health
            .as_ref()
            .filter(|h| !h.healthy)
            .map(|h| format!("\n\nStato container del progetto:\n```\n{}\n```", h.summary))
            .unwrap_or_default();
        let content = if ok {
            format!(
                "Il comando in background \"{label}\" e' terminato con SUCCESSO (exit_code={}).\n\n\
                 Output finale:\n```\n{tail}\n```\n\n\
                 Aggiorna l'utente sull'esito e prosegui con il prossimo passo, se previsto.",
                exit_code.unwrap_or(0)
            )
        } else {
            format!(
                "Il comando in background \"{label}\" e' FALLITO (exit_code={}).{docker_note}\n\n\
                 Output finale:\n```\n{tail}\n```\n\n\
                 Analizza l'errore nell'output e proponi (o applica) la correzione. \
                 NON ri-lanciare lo stesso comando senza prima aver capito e mitigato la causa \
                 (es. container in crash-loop, dipendenze non pronte, porta gia' occupata).",
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
            // Eredita la modalita' scelta dall'utente per la sessione (mig 0371)
            // invece di hardcodare Confirm: un run risvegliato in Automatico non
            // deve tornare a chiedere conferme.
            automation_mode: crate::chat_messages::read_session_automation_mode(&state.db, session_id).await,
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
    fn riconosce_docker_compose() {
        assert!(is_docker_compose_command("docker compose up"));
        assert!(is_docker_compose_command("docker compose -f compose.yml up --build"));
        assert!(is_docker_compose_command("docker-compose up -d"));
        assert!(!is_docker_compose_command("docker compose ps"));
        assert!(!is_docker_compose_command("docker run nginx"));
        assert!(!is_docker_compose_command("pnpm dev"));
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
