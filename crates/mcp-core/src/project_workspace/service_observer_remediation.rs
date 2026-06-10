//! Remediation (capacita' 1): trigger automatico dell'agente Debugger su crash
//! runtime rilevato dal service_observer.
//!
//! Gated da `agent.observer.auto_diagnose_enabled` (default false). Anti-loop:
//! cooldown per firma errore + cap orario per progetto, su `service_diagnoses`.
//! Scope rigoroso al progetto (regola E). Riusa il punto unico di avvio agente
//! `spawn_agent_run` (regola L), inserendo un messaggio sintetico con un prompt
//! di debug esplicito (stesso pattern di "Risolvi con Nexus").

use nexus_events::event::ProjectEvent;
use uuid::Uuid;

use crate::agent_types::SupervisorMode;
use crate::chat_messages::{insert_message, spawn_agent_run, SpawnAgentParams, SpawnOutcome};
use crate::orchestrator::AutomationMode;
use crate::AppState;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn maybe_trigger_debugger(
    state: &AppState,
    auto_enabled: bool,
    cooldown_s: i64,
    max_per_hour: i64,
    project_id: Uuid,
    unit: &str,
    kind: &str,
    last_log: &str,
    sig: &str,
    diag_id: Option<Uuid>,
) {
    if !auto_enabled {
        return;
    }

    // Cooldown: stessa firma gia' diagnosticata entro la finestra.
    let recent: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM service_diagnoses \
         WHERE project_id = $1 AND unit = $2 AND error_signature_hash = $3 \
           AND triggered_run_id IS NOT NULL \
           AND ts > NOW() - make_interval(secs => $4)",
    )
    .bind(project_id)
    .bind(unit)
    .bind(sig)
    .bind(cooldown_s as f64)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    if recent > 0 {
        return;
    }

    // Cap orario per progetto (anti-loop di run costosi).
    let last_hour: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM service_diagnoses \
         WHERE project_id = $1 AND triggered_run_id IS NOT NULL \
           AND ts > NOW() - INTERVAL '1 hour'",
    )
    .bind(project_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    if last_hour >= max_per_hour {
        tracing::warn!(
            "service_observer: cap diagnosi/ora ({}) raggiunto per progetto {}, skip auto-debug",
            max_per_hour,
            project_id
        );
        return;
    }

    // Owner del progetto (user_id per l'agent run).
    let owner: Option<Uuid> =
        sqlx::query_scalar("SELECT owner_user_id FROM projects WHERE id = $1")
            .bind(project_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    let owner = match owner {
        Some(o) => o,
        None => return,
    };

    // Sessione chat esistente del progetto (non ne creiamo: l'auto-debug e'
    // opt-in e ha senso solo dove l'utente vede la conversazione).
    let session: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM chat_sessions WHERE project_id = $1 ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    let session = match session {
        Some(s) => s,
        None => {
            tracing::info!(
                "service_observer: nessuna sessione chat per progetto {}, skip auto-debug",
                project_id
            );
            return;
        }
    };

    let content = format!(
        "Crash rilevato automaticamente nel servizio `{unit}` (tipo: {kind}).\n\n\
         Log rilevante:\n```\n{last_log}\n```\n\n\
         Diagnostica la causa radice con metodo scientifico (ipotesi -> falsificazione \
         -> fix -> verifica): leggi i log completi e i file coinvolti prima di concludere, \
         poi proponi la correzione."
    );
    let meta = serde_json::json!({
        "kind": "auto_diagnosis",
        "synthetic": true,
        "service_unit": unit,
        "error_kind": kind,
        "source": "service_observer",
    });

    let user_message_id =
        match insert_message(&state.db, session, project_id, "user", &content, meta, None).await {
            Ok(id) => id,
            Err(_) => {
                tracing::warn!("service_observer: insert messaggio sintetico fallito");
                return;
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
        session_id: session,
        project_id,
        user_message_id,
        content,
        // Eredita la modalita' della sessione (mig 0371) invece di hardcodare
        // Confirm, cosi' l'auto-debug rispetta la scelta dell'utente.
        automation_mode: crate::chat_messages::read_session_automation_mode(&state.db, session)
            .await,
        supervisor_mode: SupervisorMode::None,
        profile_prompt_block: String::new(),
        system_context,
        provider_override: None,
        model_override: None,
        profile_provider: None,
        profile_model: None,
        attachments: Vec::new(),
        user_role: "system".to_string(),
        // Forza l'agente Debugger (il prompt esplicito nel content rinforza).
        nexus_agent_type_hint: Some("debugger".to_string()),
    };

    match spawn_agent_run(state, params).await {
        SpawnOutcome::Started(result) => {
            let run_id = result.run_id;
            if let Some(id) = diag_id {
                let _ = sqlx::query(
                "UPDATE service_diagnoses SET status = 'diagnosing', triggered_run_id = $1 WHERE id = $2",
            )
            .bind(run_id)
            .bind(id)
            .execute(&state.db)
            .await;
            }
            nexus_events::dispatcher::emit_global(
                project_id,
                ProjectEvent::ServiceDiagnosisStarted {
                    unit: unit.to_string(),
                    run_id: run_id.to_string(),
                },
            );
            tracing::info!(
                "service_observer: auto-debug avviato run={} per {} ({})",
                run_id,
                unit,
                kind
            );

            // Auto-remediation loop (chiusura del ciclo): quando il debugger termina,
            // riavvia il servizio cosi' l'observer al ciclo successivo ri-verifica la
            // readiness. Se il servizio e' UP -> resolve; se ancora giu' con causa
            // DIVERSA (nuova firma) -> nuovo trigger. Il cap orario + cooldown per
            // firma sono il freno anti-loop. Task in background: non blocca l'observer.
            let state_cl = state.clone();
            let unit_cl = unit.to_string();
            tokio::spawn(async move {
                // Attende la fine del run debugger (max ~5 min), poi riavvia.
                for _ in 0..60u32 {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    let status: Option<String> =
                        sqlx::query_scalar("SELECT status FROM agent_runs WHERE id = $1")
                            .bind(run_id)
                            .fetch_optional(&state_cl.db)
                            .await
                            .ok()
                            .flatten();
                    match status.as_deref() {
                        // run ancora in corso: continua ad attendere
                        Some("running") | Some("awaiting_confirmation") | None => continue,
                        // terminato (completed/failed/...): procedi al riavvio
                        _ => break,
                    }
                }
                crate::project_workspace::services::restart_project_unit(
                    &state_cl, project_id, &unit_cl,
                )
                .await;
            });
        }
        SpawnOutcome::Disambiguation(_) => {
            // Non dovrebbe accadere: passiamo nexus_agent_type_hint="debugger"
            // che forza l'AgentType e salta il gate di disambiguazione.
            tracing::warn!(
                "service_observer: disambiguazione inattesa per {} ({}), nessun auto-debug avviato",
                unit,
                kind
            );
        }
        SpawnOutcome::NotStarted => {
            // Progetto non caricabile o altro fallback: nessun run (come prima).
        }
    }
}
