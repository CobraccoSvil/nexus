//! Port enforcer: task background che scansiona ogni 5s le porte TCP in ascolto
//! e killa processi di progetto che bindano porte fuori dal loro bucket assegnato.
//!
//! Flusso:
//! 1. `detect_all_port_bindings(db)` ottiene tutte le porte LISTEN con PID e project_id
//! 2. Per ogni binding con project_id noto:
//!    a. Controlla se la porta e' nel bucket deterministico del progetto
//!    b. Oppure se e' allocata in `nexus_port_allocations` per quel progetto
//!    c. Se no: SIGTERM → 2s → SIGKILL + audit `port_violation_kill` + Notification
//!
//! Le porte senza project_id (processi non-progetto) vengono ignorate:
//! il port_enforcer protegge solo i confini tra progetti, non il sistema in generale.

use std::time::Duration;

use serde_json::json;
use uuid::Uuid;

use crate::project_workspace::services::{
    detect_all_port_bindings, port_allocated_to_project, port_in_project_bucket,
    project_bucket_range,
};
use crate::security::{record_audit, AuditEntry};

/// Intervallo di scansione: 5s bilancia reattivita' (finestra di violazione breve)
/// con overhead CPU (<0.1% su ss + /proc scan).
const SCAN_INTERVAL: Duration = Duration::from_secs(5);

/// Loop principale: chiamato da `main.rs` startup via `tokio::spawn(...)`.
/// Richiede il pool DB per cross-referencing PID-progetto e il registry dei
/// canali per emettere notifiche real-time al frontend.
/// Timeout per singola iterazione di scan: se supera questo limite la scan
/// viene abortita e il loop continua. Protegge il runtime tokio da blocchi
/// in /proc o query DB lente.
const SCAN_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn port_enforcer_loop(state: crate::AppState) {
    let mut ticker = tokio::time::interval(SCAN_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tracing::info!(
        "port_enforcer avviato (scan ogni {}s)",
        SCAN_INTERVAL.as_secs()
    );
    loop {
        ticker.tick().await;
        match tokio::time::timeout(SCAN_TIMEOUT, scan_and_enforce(&state)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("port_enforcer scan fallito: {e}"),
            Err(_) => tracing::error!(
                "port_enforcer scan timeout ({}s): iterazione abortita",
                SCAN_TIMEOUT.as_secs()
            ),
        }
    }
}

/// Singola iterazione di scansione + enforcement.
async fn scan_and_enforce(state: &crate::AppState) -> Result<(), String> {
    let db = &state.db;
    let channels = &state.project_channels;
    let bindings = detect_all_port_bindings(db).await?;

    // Violazioni osservate in QUESTO scan: alimentano lo sweep di chiusura in
    // coda (le diagnosi runtime aperte NON piu' presenti qui vengono risolte).
    let mut current_violations: Vec<(Uuid, f64)> = Vec::new();

    for b in &bindings {
        let project_id = match b.project_id {
            Some(pid) => pid,
            None => continue, // processo non-progetto, skip
        };

        // Controlla se la porta e' lecita per questo progetto (punto unico, regola L)
        let (bucket, bucket_end) = project_bucket_range(&project_id);

        if port_in_project_bucket(&project_id, b.port) {
            continue; // porta nel bucket: ok
        }

        // Fuori dal bucket: controlla se e' allocata esplicitamente
        let owned = port_allocated_to_project(db, b.port, project_id).await;
        if owned {
            continue; // allocazione esplicita: ok
        }

        current_violations.push((project_id, b.port as f64));

        // VIOLAZIONE: il processo ha bindato una porta non autorizzata
        tracing::error!(
            pid = b.pid,
            port = b.port,
            project_id = %project_id,
            program = %b.program,
            bucket_start = bucket,
            bucket_end,
            "port_enforcer: violazione rilevata, processo terminato"
        );

        // Terminazione via punto unico cross-platform (regola L): incapsula la
        // sequenza TERM -> grace -> ricontrollo liveness -> KILL su Unix e usa
        // taskkill /T /F su Windows. Il precedente `kill` inline era no-op su
        // Windows (comando inesistente) -> violazioni di porta mai fatte rispettare.
        crate::process_util::kill_pid(b.pid).await;

        // Audit trail
        record_audit(
            AuditEntry::killed(project_id, "port_violation_kill", "port")
                .with_resource(b.port.to_string())
                .with_details(json!({
                    "pid": b.pid,
                    "port": b.port,
                    "program": b.program,
                    "bucket_start": bucket,
                    "bucket_end": bucket_end,
                })),
        );

        // Notifica real-time al frontend
        emit_violation_notification(channels, project_id, b.port);

        // Catena di governance (non solo sopprimere l'effetto): localizza il
        // sorgente della porta uccisa, apre la diagnosi policy_violation
        // (pannello Problemi, stessa firma del resource_linter: niente doppioni)
        // e innesca subito la riparazione della CAUSA. In task separato per non
        // ritardare la scansione successiva.
        let state_chain = state.clone();
        let killed_port = b.port;
        let program = b.program.clone();
        tokio::spawn(async move {
            chain_violation_to_remediation(&state_chain, project_id, killed_port, &program).await;
        });
    }

    // Ciclo di vita delle violazioni porta RUNTIME (regola H: niente fantasmi
    // eterni nel pannello Problemi): le diagnosi aperte senza sorgente
    // localizzato la cui porta non risulta piu' in violazione vengono risolte.
    // Copre sia le violazioni rientrate davvero (processo terminato/riallocato)
    // sia i falsi positivi storici da PID riciclato che nessun linter per-file
    // avrebbe mai richiuso.
    //
    // Guardia FAIL-CLOSED (coerente col GC porte, memoria servizi-porte): eseguo
    // lo sweep SOLO se il rilevamento ha visto almeno una porta in ascolto. Un
    // `bindings` vuoto su un sistema reale (che ha sempre DB/servizi in ascolto)
    // segnala un rilevamento cieco (Get-NetTCPConnection/ss transitoriamente a
    // vuoto), non "zero porte": chiudere le violazioni in quel caso sarebbe un
    // fail-open che le farebbe sparire e riapparire (flicker) ogni volta che il
    // probe fallisce. Con almeno un binding il rilevamento e' affidabile e
    // l'assenza di una violazione significa che e' davvero rientrata.
    if !bindings.is_empty() {
        let resolved = crate::security::resource_governance::resolve_stale_runtime_port_violations(
            db,
            &current_violations,
        )
        .await;
        if !resolved.is_empty() {
            tracing::info!(
                resolved = resolved.len(),
                "port_enforcer: violazioni porta runtime non piu' osservate risolte"
            );
            crate::project_workspace::logs::emit_problems_panel_refresh_batch(&resolved);
        }
    }

    Ok(())
}

/// Dopo un kill: apre la diagnosi della violazione (localizzando il sorgente
/// se possibile) e avvia la riparazione automatica.
async fn chain_violation_to_remediation(
    state: &crate::AppState,
    project_id: Uuid,
    port: u16,
    program: &str,
) {
    let root: Option<String> =
        sqlx::query_scalar("SELECT repository_root_path FROM projects WHERE id = $1")
            .bind(project_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    let allocated =
        crate::security::resource_linter::legitimate_ports_for_project(&state.db, project_id).await;

    // Localizza il sorgente della porta (best-effort).
    let file_finding = if let Some(root) = root.filter(|r| !r.is_empty()) {
        let root_path = std::path::PathBuf::from(root);
        let alloc = allocated.clone();
        tokio::task::spawn_blocking(move || {
            crate::security::resource_linter::lint_tree_for_port(&root_path, &alloc, port as u32)
        })
        .await
        .unwrap_or_default()
        .into_iter()
        .next()
    } else {
        None
    };

    let (kind, rule) = ("port", "require_allocation");
    let (file_path, line, snippet) = match &file_finding {
        Some(f) => (Some(f.rel_path.as_str()), f.line, f.snippet.clone()),
        None => (None, 0, String::new()),
    };
    let sig = crate::security::resource_governance::violation_signature(
        project_id,
        file_path,
        &port.to_string(),
        &format!("{kind}/{rule}"),
    );
    let detail = match file_path {
        Some(p) => format!(
            "{p}:{line} porta {port} non allocata (processo '{program}' terminato dal port enforcer) | {snippet}"
        ),
        None => format!(
            "porta {port} non allocata bindata a runtime (processo '{program}' terminato dal port enforcer); sorgente non localizzato"
        ),
    };
    crate::security::resource_governance::open_resource_violation(
        &state.db,
        project_id,
        kind,
        rule,
        port as f64,
        file_path,
        &detail,
        &sig,
    )
    .await;
    crate::project_workspace::resource_violation_remediation::process_open_violations(
        state, project_id,
    )
    .await;
}

/// Emette una notifica di violazione sul canale del progetto.
fn emit_violation_notification(
    channels: &nexus_events::ProjectChannels,
    project_id: Uuid,
    port: u16,
) {
    // Gli estremi vengono dal punto unico (regola L): il chiamante non li passa,
    // cosi' non puo' passarne di diversi da quelli su cui si e' deciso il kill.
    let (bucket_start, bucket_end) = project_bucket_range(&project_id);
    nexus_events::dispatcher::emit(
        channels,
        project_id,
        nexus_events::ProjectEvent::Notification {
            severity: "error".into(),
            message: format!(
                "Servizio terminato: porta {} fuori dal bucket progetto [{}, {}]. \
                 Usa request_port per allocare porte autorizzate.",
                port, bucket_start, bucket_end
            ),
            panel: Some("services".into()),
            ttl_ms: Some(15000),
            run_id: None,
        },
    );
}

// La terminazione dei processi in violazione e' delegata al punto unico
// cross-platform `crate::process_util::kill_pid` (regola L): l'enum `Signal` e la
// funzione `kill_process` locali (che invocavano `kill` inline, no-op su Windows)
// sono state rimosse in favore di quella singola implementazione. La sequenza
// TERM -> grace -> KILL su Unix e il taskkill /T /F su Windows vivono li'.
