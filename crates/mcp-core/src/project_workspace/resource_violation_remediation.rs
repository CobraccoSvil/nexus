//! Riparazione automatica delle violazioni di governance risorse (porte/URL
//! hardcoded nei sorgenti). Gemello di `service_observer_remediation` per le
//! diagnosi `signal_kind='policy_violation'` aperte dal `resource_linter` o
//! dalla catena del `port_enforcer` (kill runtime -> correzione della causa).
//!
//! Solo violazioni CORREGGIBILI in modo deterministico (decisione utente
//! 2026-06-11): la policy `auto_remediate` del catalogo (mig 0397) decide per
//! regola; le classi a solo blocco (db/fs/container) non passano mai di qui.
//!
//! Anti-loop: SOLO violazioni con un sorgente editabile (`file_path` reale)
//! passano di qui; le violazioni RUNTIME del `port_enforcer` (kill di un processo
//! che ha aperto una porta fuori bucket: `file_path` NULL o "-") NON hanno un file
//! da correggere e venivano rilanciate all'infinito (la porta fuori-bucket cambia
//! a ogni kill -> firma sempre nuova -> il cap-per-firma non converge mai). Oltre
//! a questo: flag globale + cooldown per firma + cap orario + cap tentativi per
//! firma su finestra 24h cross-row -> stato terminale `failed_remediation` con
//! notifica esplicita (il problema resta visibile nel pannello Problemi).
//! Prompt fuori-chat: template DB `agent.resource_violation.remediation`
//! (mig 0399, regola D). Punto unico di avvio: `spawn_agent_run` (regola L).

use nexus_events::event::ProjectEvent;
use uuid::Uuid;

use crate::agent_types::SupervisorMode;
use crate::chat_messages::{
    insert_message, session_has_active_run, spawn_agent_run, SpawnAgentParams, SpawnOutcome,
};
use crate::AppState;

/// Riga di violazione aperta (subset di service_diagnoses).
#[derive(Debug, Clone)]
pub(crate) struct ViolationRow {
    pub id: Uuid,
    pub file_path: Option<String>,
    pub detail: String,
    pub signature: String,
}

/// Esito del gating (funzione pura, testabile).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GateDecision {
    Proceed,
    Disabled,
    HourlyCapReached,
    AttemptsExhausted,
}

pub(crate) fn remediation_gate(
    enabled: bool,
    runs_last_hour: i64,
    max_per_hour: i64,
    attempts_for_sig: i64,
    max_attempts: i64,
) -> GateDecision {
    if !enabled {
        return GateDecision::Disabled;
    }
    if runs_last_hour >= max_per_hour {
        return GateDecision::HourlyCapReached;
    }
    if attempts_for_sig >= max_attempts {
        return GateDecision::AttemptsExhausted;
    }
    GateDecision::Proceed
}

/// Vero se la violazione e' riparabile editando un sorgente: deve avere un
/// `file_path` reale. Le violazioni RUNTIME (il `port_enforcer` killa un processo
/// che ha aperto una porta fuori dal bucket: `file_path` NULL o "-") NON hanno un
/// sorgente da correggere; lanciare un run agente di remediation-by-edit e'
/// inutile e genera un loop costoso (kill -> diagnosi runtime -> il re-lint non
/// trova nulla da sistemare -> la diagnosi resta -> si ripete; per giunta la porta
/// fuori-bucket cambia ogni volta, quindi il cap-per-firma non scatta mai). Quelle
/// restano come diagnosi nel pannello, senza bruciare un run. Funzione pura
/// (testabile), punto unico del criterio (regola L).
pub(crate) fn violation_is_remediable_by_edit(file_path: Option<&str>) -> bool {
    matches!(file_path, Some(p) if !p.is_empty() && p != "-")
}

/// Rendering del template di riparazione (funzione pura, testabile).
pub(crate) fn build_remediation_content(
    template: &str,
    violations: &[ViolationRow],
    bucket: (u16, u16),
    allocations: &[(i32, String)],
) -> String {
    let violations_text = violations
        .iter()
        .map(|v| format!("- {}", v.detail))
        .collect::<Vec<_>>()
        .join("\n");
    let alloc_text = if allocations.is_empty() {
        "(nessuna porta ancora allocata: usa request_port)".to_string()
    } else {
        allocations
            .iter()
            .map(|(p, l)| {
                let label = if l.is_empty() { "(senza label)" } else { l };
                format!("- {p} -> {label}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    template
        .replace("{violations}", &violations_text)
        .replace("{bucket_start}", &bucket.0.to_string())
        .replace("{bucket_end}", &bucket.1.to_string())
        .replace("{allocated_ports}", &alloc_text)
}

/// Legge le violazioni `policy_violation` aperte del progetto e, se il gating
/// lo consente, avvia UN solo run di riparazione che le copre tutte. Chiusura:
/// a fine run re-lint dei file coinvolti -> resolved / ri-open / failed.
pub(crate) async fn process_open_violations(state: &AppState, project_id: Uuid) {
    // Boot-grace (punto unico dell'osservazione servizi, regola L): dopo un
    // restart di mcp-core il re-lint rivede le violazioni note e questo worker
    // risvegliava la chat entro pochi minuti dal deploy ("la chat riparte da
    // sola a ogni ricompilazione": risvegli 19:48/19:53 attorno al restart
    // 19:50 del 20/07). Il gemello service_observer_remediation aveva gia' la
    // guardia; qui mancava.
    if super::service_observer_remediation::within_boot_grace(state).await {
        tracing::info!(
            project_id = %project_id,
            "resource_violation_remediation: boot-grace attivo, giro saltato"
        );
        return;
    }
    // Solo regole con auto_remediate=true nel catalogo (le altre restano
    // visibili nel pannello, azionabili a mano col pulsante chat).
    let enabled_flag =
        crate::settings::get_setting(&state.db, "agent.resource_violation.auto_remediate")
            .await
            .ok()
            .flatten()
            .map(|v| {
                !matches!(
                    v.trim().to_lowercase().as_str(),
                    "false" | "0" | "off" | "no"
                )
            })
            .unwrap_or(true);

    let cooldown_s: i64 = crate::settings::get_setting(
        &state.db,
        "agent.resource_violation.remediate_cooldown_seconds",
    )
    .await
    .ok()
    .flatten()
    .and_then(|v| v.trim().parse().ok())
    .unwrap_or(900);
    let max_per_hour: i64 =
        crate::settings::get_setting(&state.db, "agent.resource_violation.remediate_max_per_hour")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(3);
    let max_attempts: i64 = crate::settings::get_setting(
        &state.db,
        "agent.resource_violation.max_attempts_per_signature",
    )
    .await
    .ok()
    .flatten()
    .and_then(|v| v.trim().parse().ok())
    .unwrap_or(2);

    // Violazioni aperte, escluse quelle in cooldown e quelle di regole non
    // auto-riparabili (join col catalogo: metric = 'kind/rule').
    let rows: Vec<(Uuid, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT d.id, d.file_path, d.detail, d.error_signature_hash \
               FROM service_diagnoses d \
               JOIN nexus_resource_policies p \
                 ON p.resource_kind = split_part(d.metric, '/', 1) \
                AND p.rule_key = split_part(d.metric, '/', 2) \
              WHERE d.project_id = $1 \
                AND d.signal_kind = 'policy_violation' \
                AND d.status = 'open' \
                AND p.enabled AND p.auto_remediate \
                AND (d.cooldown_until IS NULL OR d.cooldown_until < NOW()) \
                AND d.file_path IS NOT NULL AND d.file_path <> '-' AND d.file_path <> '' \
              ORDER BY d.ts ASC \
              LIMIT 20",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    if rows.is_empty() {
        return;
    }
    let violations: Vec<ViolationRow> = rows
        .into_iter()
        // Difesa in profondita' (oltre al filtro SQL): solo le violazioni con un
        // sorgente editabile passano alla remediation-by-edit; le runtime senza
        // file (kill del port_enforcer) restano diagnosi, niente run agente.
        .filter(|(_, file_path, _, _)| violation_is_remediable_by_edit(file_path.as_deref()))
        .map(|(id, file_path, detail, sig)| ViolationRow {
            id,
            file_path,
            detail: detail.unwrap_or_default(),
            signature: sig.unwrap_or_default(),
        })
        .collect();

    // Cap orario (run di riparazione gia' avviati nell'ultima ora).
    let runs_last_hour: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM service_diagnoses \
          WHERE project_id = $1 AND signal_kind = 'policy_violation' \
            AND triggered_run_id IS NOT NULL AND ts > NOW() - INTERVAL '1 hour'",
    )
    .bind(project_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    // Cap tentativi su 24h CROSS-ROW, contato PER FILE e non per firma puntuale
    // (regola M): la firma include il VALUE (porta/URL) e un rimedio parziale che
    // sposta la porta genera una firma NUOVA a ogni giro -> il cap non convergeva
    // mai (provato sul progetto vendita-immobile: 07:25 e 07:30, due run su
    // frontend/vite.config.ts con firme diverse). La remediation e' by-edit sul
    // FILE: se quel file ha gia' consumato i tentativi, altri run identici non
    // aggiungono nulla, comunque si sia spostato il valore.
    let mut max_sig_attempts: i64 = 0;
    for v in &violations {
        let Some(fp) = v.file_path.as_deref().filter(|p| !p.is_empty()) else {
            continue;
        };
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM service_diagnoses \
              WHERE project_id = $1 AND signal_kind = 'policy_violation' \
                AND file_path = $2 AND triggered_run_id IS NOT NULL \
                AND ts > NOW() - INTERVAL '24 hours'",
        )
        .bind(project_id)
        .bind(fp)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
        max_sig_attempts = max_sig_attempts.max(n);
    }

    match remediation_gate(
        enabled_flag,
        runs_last_hour,
        max_per_hour,
        max_sig_attempts,
        max_attempts,
    ) {
        GateDecision::Proceed => {}
        GateDecision::Disabled => return,
        GateDecision::HourlyCapReached => {
            tracing::warn!(
                project_id = %project_id,
                "resource_remediation: cap orario raggiunto, riparazione rimandata"
            );
            return;
        }
        GateDecision::AttemptsExhausted => {
            mark_failed_remediation(state, project_id, &violations).await;
            return;
        }
    }

    // Owner + ultima sessione chat (pattern maybe_trigger_debugger).
    let owner: Option<Uuid> =
        sqlx::query_scalar("SELECT owner_user_id FROM projects WHERE id = $1")
            .bind(project_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    let Some(owner) = owner else { return };
    // Separazione DB: chat_sessions e' una tabella migrata -> instrada sul pool
    // del progetto. Non disponibile -> niente remediation per questo giro
    // (trigger best-effort, WARN + return).
    let proj_pool =
        match crate::project_db_routes::project_data_pool_from(&state.db, project_id).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    project_id = %project_id,
                    error = %e,
                    "resource_remediation: DB progetto non disponibile, skip riparazione"
                );
                return;
            }
        };
    let session: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM chat_sessions WHERE project_id = $1 ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(&proj_pool)
    .await
    .ok()
    .flatten();
    let Some(session) = session else {
        tracing::info!(
            "resource_remediation: nessuna sessione chat per progetto {project_id}, violazioni restano nel pannello"
        );
        return;
    };

    // Un run e' GIA' attivo sulla sessione (tipico: la creazione dell'app che sta
    // SCRIVENDO il codice con una porta hardcoded -> apre la violazione): NON
    // spawnare la riparazione, la supererebbe via supersede (last-wins) uccidendo
    // il run in corso mentre progrediva (incidente ricreazione vendita-immobile:
    // il run di creazione cancellato da `superseded_by_new_run` a meta' lavoro).
    // Gemello del guard di process_resume:377 e service_observer_remediation:151;
    // qui MANCAVA. La violazione resta aperta e la riparazione ritenta a sessione
    // libera. proj_pool: agent_runs e' tabella per-progetto (separazione DB).
    if session_has_active_run(&proj_pool, session).await {
        tracing::debug!(
            "resource_remediation: run gia' attivo sulla sessione {session}, riparazione rimandata a sessione libera"
        );
        return;
    }

    // Template fuori-chat (regola D) + contesto porte.
    let template = crate::prompt_templates::get_template_or_default(
        &state.db,
        &state.template_cache,
        "agent.resource_violation.remediation",
    )
    .await;
    if !template.contains("{violations}") {
        tracing::warn!(
            "resource_remediation: template agent.resource_violation.remediation senza placeholder, skip"
        );
        return;
    }
    let (bucket_start, bucket_end) =
        crate::project_workspace::services::project_bucket_range(&project_id);
    let allocations: Vec<(i32, String)> = sqlx::query_as(
        "SELECT port, label FROM nexus_port_allocations WHERE project_id = $1 ORDER BY port",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let content = build_remediation_content(
        &template,
        &violations,
        (bucket_start, bucket_end),
        &allocations,
    );
    let meta = serde_json::json!({
        "kind": "resource_violation_remediation",
        "synthetic": true,
        "source": "resource_governance",
        "diagnosis_ids": violations.iter().map(|v| v.id.to_string()).collect::<Vec<_>>(),
    });

    let user_message_id =
        match insert_message(&state.db, session, project_id, "user", &content, meta, None).await {
            Ok(id) => id,
            Err(_) => return,
        };
    let system_context = crate::prompt_templates::get_template_or_default(
        &state.db,
        &state.template_cache,
        "system.nexus_base",
    )
    .await;

    // Modello del rimedio dal purpose tier-aware 'auto_remediation' (mig 0626,
    // regola G): vedi service_observer_remediation, stesso punto unico.
    // Se non risolvibile -> (None, None): routing di default, rimedio mai bloccato.
    let (provider_override, model_override) = crate::internal_routing::purpose_override_or_default(
        state,
        crate::internal_routing::PURPOSE_AUTO_REMEDIATION,
    )
    .await;

    let params = SpawnAgentParams {
        user_id: owner,
        session_id: session,
        project_id,
        user_message_id,
        content,
        // chat_sessions e' migrata: la modalita' della sessione va letta dal
        // pool progetto gia' risolto sopra, non dal meta (sempre default).
        automation_mode: crate::chat_messages::read_session_automation_mode(&proj_pool, session)
            .await,
        supervisor_mode: SupervisorMode::None,
        profile_prompt_block: String::new(),
        system_context,
        // Il provider del purpose e' una scelta del SISTEMA, non un ordine
        // dell'utente: entra come preferenza (il rimedio non deve restare senza
        // fornitore se quello del purpose e' caduto). Punto unico della scelta,
        // dove il pin per costruzione non puo' nascere.
        provider_choice: crate::orchestrator::ProviderChoice::resolve(
            None,
            crate::orchestrator::ProviderOverrideMode::Preferred,
            provider_override.as_deref(),
        ),
        model_override,
        profile_provider: None,
        profile_model: None,
        attachments: Vec::new(),
        nexus_agent_type_hint: Some("debugger".to_string()),
    };

    match spawn_agent_run(state, params).await {
        SpawnOutcome::Started(result) => {
            let run_id = result.run_id;
            let ids: Vec<Uuid> = violations.iter().map(|v| v.id).collect();
            let _ = sqlx::query(
                "UPDATE service_diagnoses \
                    SET status = 'diagnosing', triggered_run_id = $1, \
                        remediation_attempts = remediation_attempts + 1, \
                        cooldown_until = NOW() + make_interval(secs => $2) \
                  WHERE id = ANY($3)",
            )
            .bind(run_id)
            .bind(cooldown_s as f64)
            .bind(&ids)
            .execute(&state.db)
            .await;
            nexus_events::dispatcher::emit_global(
                project_id,
                ProjectEvent::Notification {
                    severity: "warning".to_string(),
                    message: format!(
                        "Riparazione automatica violazioni risorse avviata ({} violazione/i, run {run_id})",
                        violations.len()
                    ),
                    panel: Some("problems".to_string()),
                    ttl_ms: Some(15_000),
                    run_id: Some(run_id.to_string()),
                },
            );
            crate::security::record_audit(
                crate::security::AuditEntry::allowed(
                    project_id,
                    "resource_violation_remediation",
                    "port",
                )
                .with_details(serde_json::json!({
                    "run_id": run_id.to_string(),
                    "violations": violations.len(),
                })),
            );
            tracing::info!(
                "resource_remediation: run {run_id} avviato per {} violazioni (progetto {project_id})",
                violations.len()
            );

            // Chiusura del ciclo: attesa fine run, poi re-lint e stato finale.
            let state_cl = state.clone();
            tokio::spawn(async move {
                // Separazione DB: agent_runs e' migrata -> pool del progetto
                // (project_id in scope). Non disponibile -> il task best-effort
                // termina con WARN (niente attesa/re-lint per questo run).
                let runs_pool = match crate::project_db_routes::project_data_pool_from(
                    &state_cl.db,
                    project_id,
                )
                .await
                {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(
                            project_id = %project_id,
                            error = %e,
                            "resource_remediation: DB progetto non disponibile, salto attesa e re-lint post-run"
                        );
                        return;
                    }
                };
                for _ in 0..60u32 {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    let status: Option<String> =
                        sqlx::query_scalar("SELECT status FROM agent_runs WHERE id = $1")
                            .bind(run_id)
                            .fetch_optional(&runs_pool)
                            .await
                            .ok()
                            .flatten();
                    match status.as_deref() {
                        // run ancora attivo/sospeso-vivo (punto unico regola L): attendi
                        Some(s) if crate::agent_types::is_active_run_status(s) => continue,
                        None => continue,
                        _ => break,
                    }
                }
                close_after_remediation(&state_cl, project_id, run_id).await;
            });
        }
        SpawnOutcome::Disambiguation(_) | SpawnOutcome::NotStarted => {
            tracing::warn!(
                "resource_remediation: run non avviato per progetto {project_id} (disambiguazione/not started)"
            );
        }
    }
}

/// Reaper one-shot all'avvio: chiude le diagnosi rimaste `diagnosing` da un
/// run di rimedio MORTO. La chiusura normale (attesa fine run -> re-lint in
/// `close_after_remediation`) vive in un task IN MEMORIA: se mcp-core viene
/// riavviato/crasha con un run di rimedio in volo, il task sparisce e la
/// diagnosi resta `diagnosing` per sempre (zombie osservati sul progetto
/// vendita-immobile, ferme al 20/07 dopo il crash). Il criterio e' il segnale
/// strutturato dello stato del run (regola M: `is_active_run_status`, punto
/// unico), mai l'eta' della riga; la chiusura passa dallo STESSO punto unico
/// del flusso normale (`close_after_remediation` -> re-lint reale, regola L/O).
pub(crate) fn spawn_stale_diagnosing_reaper(state: AppState) {
    tokio::spawn(async move {
        // Breve attesa: lascia stabilizzare pool e registry dopo il boot.
        tokio::time::sleep(std::time::Duration::from_secs(45)).await;
        reap_stale_diagnosing(&state).await;
    });
}

async fn reap_stale_diagnosing(state: &AppState) {
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT DISTINCT project_id, triggered_run_id FROM service_diagnoses \
          WHERE signal_kind = 'policy_violation' AND status = 'diagnosing' \
            AND triggered_run_id IS NOT NULL",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    for (project_id, run_id) in rows {
        if remediation_run_is_active(state, project_id, run_id).await {
            continue; // il task di chiusura del run corrente se ne occupera'
        }
        tracing::info!(project_id = %project_id, run_id = %run_id,
            "diagnosing_reaper: run di rimedio terminato/assente, chiudo via re-lint");
        close_after_remediation(state, project_id, run_id).await;
    }
}

/// True se il run di rimedio e' ancora ATTIVO sul pool del progetto (segnale
/// strutturato `is_active_run_status`, regola M). DB progetto non disponibile
/// -> true prudente (non chiudere: al prossimo boot si rivaluta).
async fn remediation_run_is_active(state: &AppState, project_id: Uuid, run_id: Uuid) -> bool {
    let runs_pool =
        match crate::project_db_routes::project_data_pool_from(&state.db, project_id).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(project_id = %project_id, error = %e,
                    "diagnosing_reaper: DB progetto non disponibile, salto");
                return true;
            }
        };
    let status: Option<String> =
        sqlx::query_scalar("SELECT status FROM agent_runs WHERE id = $1")
            .bind(run_id)
            .fetch_optional(&runs_pool)
            .await
            .ok()
            .flatten();
    status
        .as_deref()
        .is_some_and(crate::agent_types::is_active_run_status)
}

/// Re-lint post-run: per ogni diagnosi `diagnosing` di questo run, verifica se
/// la violazione e' sparita dai sorgenti -> resolved; altrimenti ri-open (il
/// cap tentativi al prossimo giro decide l'eventuale failed_remediation).
async fn close_after_remediation(state: &AppState, project_id: Uuid, run_id: Uuid) {
    let root: Option<String> =
        sqlx::query_scalar("SELECT repository_root_path FROM projects WHERE id = $1")
            .bind(project_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    let Some(root) = root.filter(|r| !r.is_empty()) else {
        return;
    };
    let allocated =
        crate::security::resource_linter::legitimate_ports_for_project(&state.db, project_id).await;
    let root_path = std::path::PathBuf::from(&root);
    let alloc_clone = allocated.clone();
    let findings = tokio::task::spawn_blocking(move || {
        crate::security::resource_linter::lint_tree(&root_path, &alloc_clone)
    })
    .await
    .map(|t| t.findings)
    .unwrap_or_default();

    let open_rows: Vec<(Uuid, String, Option<String>, f64)> = sqlx::query_as(
        "SELECT id, metric, file_path, COALESCE(value, 0) FROM service_diagnoses \
          WHERE project_id = $1 AND triggered_run_id = $2 \
            AND signal_kind = 'policy_violation' AND status = 'diagnosing'",
    )
    .bind(project_id)
    .bind(run_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    for (diag_id, metric, file_path, value) in open_rows {
        let still_dirty = findings.iter().any(|f| {
            let (kind, rule) = f.kind.rule();
            format!("{kind}/{rule}") == metric
                && file_path.as_deref() == Some(f.rel_path.as_str())
                && (f.port as f64 - value).abs() < f64::EPSILON
        });
        if still_dirty {
            let _ = sqlx::query("UPDATE service_diagnoses SET status = 'open' WHERE id = $1")
                .bind(diag_id)
                .execute(&state.db)
                .await;
        } else {
            let _ = sqlx::query(
                "UPDATE service_diagnoses SET status = 'resolved', resolved_at = NOW() WHERE id = $1",
            )
            .bind(diag_id)
            .execute(&state.db)
            .await;
        }
    }
}

/// Marca le violazioni come `failed_remediation` (terminale) + notifica + audit.
async fn mark_failed_remediation(state: &AppState, project_id: Uuid, violations: &[ViolationRow]) {
    let ids: Vec<Uuid> = violations.iter().map(|v| v.id).collect();
    let updated = sqlx::query(
        "UPDATE service_diagnoses SET status = 'failed_remediation' \
          WHERE id = ANY($1) AND status <> 'failed_remediation'",
    )
    .bind(&ids)
    .execute(&state.db)
    .await
    .map(|r| r.rows_affected())
    .unwrap_or(0);
    if updated == 0 {
        return;
    }
    let files: Vec<String> = violations
        .iter()
        .filter_map(|v| v.file_path.clone())
        .collect();
    nexus_events::dispatcher::emit_global(
        project_id,
        ProjectEvent::Notification {
            severity: "error".to_string(),
            message: format!(
                "Riparazione automatica violazioni risorse FALLITA dopo i tentativi massimi: serve intervento manuale ({})",
                files.join(", ")
            ),
            panel: Some("problems".to_string()),
            ttl_ms: Some(60_000),
            run_id: None,
        },
    );
    crate::security::record_audit(crate::security::AuditEntry {
        project_id,
        actor: "system",
        actor_user_id: None,
        actor_session_id: None,
        action: "resource_violation_remediation".to_string(),
        resource_kind: "port",
        resource_id: None,
        outcome: "failed",
        details: serde_json::json!({ "files": files, "count": violations.len() }),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solo_violazioni_con_sorgente_sono_riparabili_by_edit() {
        // Violazione con sorgente reale (porta hardcoded in un file): riparabile.
        assert!(violation_is_remediable_by_edit(Some("vite.config.ts")));
        assert!(violation_is_remediable_by_edit(Some(
            "playwright.config.ts"
        )));
        // Violazioni RUNTIME (kill del port_enforcer): nessun file -> NON riparabili
        // by-edit (causa radice del loop di remediation che bruciava provider).
        assert!(!violation_is_remediable_by_edit(Some("-")));
        assert!(!violation_is_remediable_by_edit(Some("")));
        assert!(!violation_is_remediable_by_edit(None));
    }

    #[test]
    fn gate_tutte_le_ramificazioni() {
        assert_eq!(remediation_gate(false, 0, 3, 0, 2), GateDecision::Disabled);
        assert_eq!(
            remediation_gate(true, 3, 3, 0, 2),
            GateDecision::HourlyCapReached
        );
        assert_eq!(
            remediation_gate(true, 0, 3, 2, 2),
            GateDecision::AttemptsExhausted
        );
        assert_eq!(remediation_gate(true, 1, 3, 1, 2), GateDecision::Proceed);
    }

    #[test]
    fn build_content_interpola_placeholder() {
        let template = "V:\n{violations}\nB: {bucket_start}-{bucket_end}\nA:\n{allocated_ports}";
        let violations = vec![ViolationRow {
            id: Uuid::nil(),
            file_path: Some("server.js".into()),
            detail: "server.js:1 5000 (port/enforce_hardcode) | app.listen(5000)".into(),
            signature: "abc".into(),
        }];
        let out = build_remediation_content(
            template,
            &violations,
            (21950, 21999),
            &[(21968, "fullstack".into())],
        );
        assert!(out.contains("server.js:1 5000"));
        assert!(out.contains("B: 21950-21999"));
        assert!(out.contains("21968 -> fullstack"));
        assert!(!out.contains("{violations}"));
    }

    #[test]
    fn build_content_senza_allocazioni() {
        let out = build_remediation_content("{allocated_ports}", &[], (20000, 20049), &[]);
        assert!(out.contains("request_port"));
    }
}
