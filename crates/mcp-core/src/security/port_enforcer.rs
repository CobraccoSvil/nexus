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
use sqlx::PgPool;
use uuid::Uuid;

use crate::project_workspace::services::{
    detect_all_port_bindings, port_allocated_to_project, project_bucket_start,
    PROJECT_PORT_BUCKET_SIZE,
};
use crate::security::{record_audit, AuditEntry};

/// Intervallo di scansione: 5s bilancia reattivita' (finestra di violazione breve)
/// con overhead CPU (<0.1% su ss + /proc scan).
const SCAN_INTERVAL: Duration = Duration::from_secs(5);

/// Delay tra SIGTERM e SIGKILL: concede al processo 2s per cleanup.
const KILL_GRACE: Duration = Duration::from_secs(2);

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

    for b in &bindings {
        let project_id = match b.project_id {
            Some(pid) => pid,
            None => continue, // processo non-progetto, skip
        };

        // Controlla se la porta e' lecita per questo progetto
        let bucket = project_bucket_start(&project_id);
        let in_bucket = b.port >= bucket && b.port < bucket + PROJECT_PORT_BUCKET_SIZE;

        if in_bucket {
            continue; // porta nel bucket: ok
        }

        // Fuori dal bucket: controlla se e' allocata esplicitamente
        let owned = port_allocated_to_project(db, b.port, project_id).await;
        if owned {
            continue; // allocazione esplicita: ok
        }

        // VIOLAZIONE: il processo ha bindato una porta non autorizzata
        tracing::error!(
            pid = b.pid,
            port = b.port,
            project_id = %project_id,
            program = %b.program,
            bucket_start = bucket,
            bucket_end = bucket + PROJECT_PORT_BUCKET_SIZE,
            "port_enforcer: violazione rilevata, processo terminato"
        );

        // SIGTERM
        kill_process(b.pid, Signal::Term).await;

        // Attendi grace period
        tokio::time::sleep(KILL_GRACE).await;

        // Se ancora vivo: SIGKILL (check via spawn_blocking per non bloccare tokio)
        let pid_check = b.pid;
        let still_alive = tokio::task::spawn_blocking(move || process_alive(pid_check))
            .await
            .unwrap_or(false);
        if still_alive {
            kill_process(b.pid, Signal::Kill).await;
        }

        // Audit trail
        record_audit(
            AuditEntry::killed(project_id, "port_violation_kill", "port")
                .with_resource(b.port.to_string())
                .with_details(json!({
                    "pid": b.pid,
                    "port": b.port,
                    "program": b.program,
                    "bucket_start": bucket,
                    "bucket_end": bucket + PROJECT_PORT_BUCKET_SIZE,
                })),
        );

        // Notifica real-time al frontend
        emit_violation_notification(channels, project_id, b.port, bucket);

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
        crate::security::resource_linter::allocated_ports_for_project(&state.db, project_id)
            .await;

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
    bucket_start: u16,
) {
    let bucket_end = bucket_start + PROJECT_PORT_BUCKET_SIZE;
    nexus_events::dispatcher::emit(
        channels,
        project_id,
        nexus_events::ProjectEvent::Notification {
            severity: "error".into(),
            message: format!(
                "Servizio terminato: porta {} fuori dal bucket progetto [{}, {}). \
                 Usa request_port per allocare porte autorizzate.",
                port, bucket_start, bucket_end
            ),
            panel: Some("services".into()),
            ttl_ms: Some(15000),
            run_id: None,
        },
    );
}

#[derive(Debug, Clone, Copy)]
enum Signal {
    Term,
    Kill,
}

/// Invia un segnale al processo. Best-effort: errori loggati ma non propagati.
async fn kill_process(pid: u32, sig: Signal) {
    let sig_str = match sig {
        Signal::Term => "-TERM",
        Signal::Kill => "-KILL",
    };
    let result = tokio::process::Command::new("kill")
        .args([sig_str, &pid.to_string()])
        .output()
        .await;
    match result {
        Ok(out) if out.status.success() => {
            tracing::debug!(pid, signal = sig_str, "port_enforcer: segnale inviato");
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            tracing::warn!(pid, signal = sig_str, stderr = %stderr, "port_enforcer: kill fallito");
        }
        Err(e) => {
            tracing::warn!(pid, signal = sig_str, error = %e, "port_enforcer: kill errore");
        }
    }
}

/// Controlla se il processo e' ancora vivo via /proc/{pid}.
fn process_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{}", pid)).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_alive_current() {
        // Il processo corrente deve risultare vivo
        let pid = std::process::id();
        assert!(process_alive(pid));
    }

    #[test]
    fn test_process_alive_nonexistent() {
        // PID assurdo non deve risultare vivo
        assert!(!process_alive(u32::MAX));
    }

    #[test]
    fn test_signal_variants() {
        // Verifica che i variant existano (compilazione)
        let _t = Signal::Term;
        let _k = Signal::Kill;
    }
}
