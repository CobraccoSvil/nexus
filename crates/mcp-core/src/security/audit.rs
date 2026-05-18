//! Audit trail centralizzato per allocazioni/blocchi di risorse di sistema.
//!
//! Scrive in `nexus_resource_audit` in batch async per non rallentare il path
//! critico dei tool agente. Architettura:
//!
//! ```
//! tool_handler ──record(entry)──► mpsc::Sender (unbounded) ──► batch_writer_loop ──► INSERT INTO ... VALUES (...), (...), ...
//!                                                              flush ogni 100 eventi o 5s
//! ```
//!
//! `record()` e' `pub fn` non-async: scarica subito nel canale e ritorna in <1us.
//! Se il canale e' chiuso (mcp-core in shutdown), drop silenzioso (best-effort).
//!
//! Lo `start_writer(db)` va invocato UNA VOLTA in `main.rs` startup. Crea il task
//! Tokio dedicato e lo registra come consumer del canale globale.

use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Dimensione massima batch prima del flush.
const BATCH_MAX: usize = 100;
/// Intervallo massimo tra flush (anche se batch non pieno).
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub project_id: Uuid,
    pub actor: &'static str,           // "agent" | "user" | "system"
    pub actor_user_id: Option<Uuid>,
    pub actor_session_id: Option<Uuid>,
    pub action: String,                // es. "port_allocate", "command_blocked"
    pub resource_kind: &'static str,   // "port" | "db" | "container" | "file" | "env" | "command" | "service"
    pub resource_id: Option<String>,
    pub outcome: &'static str,         // "allowed" | "blocked" | "killed"
    pub details: Value,
}

impl AuditEntry {
    pub fn allowed(project_id: Uuid, action: impl Into<String>, resource_kind: &'static str) -> Self {
        Self {
            project_id,
            actor: "agent",
            actor_user_id: None,
            actor_session_id: None,
            action: action.into(),
            resource_kind,
            resource_id: None,
            outcome: "allowed",
            details: Value::Object(Default::default()),
        }
    }
    pub fn blocked(project_id: Uuid, action: impl Into<String>, resource_kind: &'static str) -> Self {
        Self {
            project_id,
            actor: "agent",
            actor_user_id: None,
            actor_session_id: None,
            action: action.into(),
            resource_kind,
            resource_id: None,
            outcome: "blocked",
            details: Value::Object(Default::default()),
        }
    }
    pub fn killed(project_id: Uuid, action: impl Into<String>, resource_kind: &'static str) -> Self {
        Self {
            project_id,
            actor: "system",
            actor_user_id: None,
            actor_session_id: None,
            action: action.into(),
            resource_kind,
            resource_id: None,
            outcome: "killed",
            details: Value::Object(Default::default()),
        }
    }
    pub fn with_resource(mut self, id: impl Into<String>) -> Self {
        self.resource_id = Some(id.into());
        self
    }
    pub fn with_details(mut self, v: Value) -> Self {
        self.details = v;
        self
    }
    pub fn with_actor_user(mut self, u: Uuid) -> Self {
        self.actor_user_id = Some(u);
        self
    }
    pub fn with_actor_session(mut self, s: Uuid) -> Self {
        self.actor_session_id = Some(s);
        self
    }
}

/// Canale globale: registrato all'avvio da `start_writer`, consumato dal task batch.
static SENDER: OnceLock<mpsc::UnboundedSender<AuditEntry>> = OnceLock::new();

/// Registra una voce di audit. Non blocca: drop silenzioso se writer non avviato
/// o canale chiuso (shutdown).
pub fn record_audit(entry: AuditEntry) {
    if let Some(tx) = SENDER.get() {
        let _ = tx.send(entry);
    } else {
        // Writer non avviato: log debug per non spammare in test
        tracing::debug!(action = %entry.action, project = %entry.project_id, "audit dropped: writer non avviato");
    }
}

/// Avvia il task background che consuma il canale e batcha le INSERT.
/// Da chiamare UNA VOLTA in `main.rs` dopo che il DB pool e' pronto.
pub fn start_writer(db: PgPool) {
    let (tx, rx) = mpsc::unbounded_channel::<AuditEntry>();
    if SENDER.set(tx).is_err() {
        tracing::warn!("security::audit::start_writer chiamato due volte: ignoro");
        return;
    }
    tokio::spawn(writer_loop(db, rx));
    tracing::info!("security::audit writer batch avviato");
}

async fn writer_loop(db: PgPool, mut rx: mpsc::UnboundedReceiver<AuditEntry>) {
    let mut buf: Vec<AuditEntry> = Vec::with_capacity(BATCH_MAX);
    let mut flush_timer = tokio::time::interval(FLUSH_INTERVAL);
    flush_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            maybe_entry = rx.recv() => {
                match maybe_entry {
                    Some(e) => {
                        buf.push(e);
                        if buf.len() >= BATCH_MAX {
                            flush(&db, &mut buf).await;
                        }
                    }
                    None => {
                        // Canale chiuso: flush finale e termina
                        flush(&db, &mut buf).await;
                        tracing::info!("security::audit writer terminato (canale chiuso)");
                        return;
                    }
                }
            }
            _ = flush_timer.tick() => {
                if !buf.is_empty() {
                    flush(&db, &mut buf).await;
                }
            }
        }
    }
}

async fn flush(db: &PgPool, buf: &mut Vec<AuditEntry>) {
    if buf.is_empty() { return; }
    // INSERT batch tramite UNNEST: piu' efficiente di N singole INSERT.
    let projects:    Vec<Uuid> = buf.iter().map(|e| e.project_id).collect();
    let actors:      Vec<&str> = buf.iter().map(|e| e.actor).collect();
    let user_ids:    Vec<Option<Uuid>> = buf.iter().map(|e| e.actor_user_id).collect();
    let session_ids: Vec<Option<Uuid>> = buf.iter().map(|e| e.actor_session_id).collect();
    let actions:     Vec<&str> = buf.iter().map(|e| e.action.as_str()).collect();
    let kinds:       Vec<&str> = buf.iter().map(|e| e.resource_kind).collect();
    let res_ids:     Vec<Option<&str>> = buf.iter().map(|e| e.resource_id.as_deref()).collect();
    let outcomes:    Vec<&str> = buf.iter().map(|e| e.outcome).collect();
    let details:     Vec<Value> = buf.iter().map(|e| e.details.clone()).collect();

    let res = sqlx::query(
        "INSERT INTO nexus_resource_audit \
         (project_id, actor, actor_user_id, actor_session_id, action, resource_kind, resource_id, outcome, details) \
         SELECT * FROM UNNEST($1::uuid[], $2::text[], $3::uuid[], $4::uuid[], $5::text[], $6::text[], $7::text[], $8::text[], $9::jsonb[])"
    )
    .bind(&projects)
    .bind(&actors)
    .bind(&user_ids)
    .bind(&session_ids)
    .bind(&actions)
    .bind(&kinds)
    .bind(&res_ids)
    .bind(&outcomes)
    .bind(&details)
    .execute(db)
    .await;

    match res {
        Ok(out) => tracing::debug!(rows = out.rows_affected(), "security::audit batch flushed"),
        Err(e) => tracing::error!(error = %e, count = buf.len(), "security::audit flush FAILED"),
    }
    buf.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_builders_produce_expected_outcomes() {
        let pid = Uuid::new_v4();
        let a = AuditEntry::allowed(pid, "port_allocate", "port").with_resource("30050");
        assert_eq!(a.outcome, "allowed");
        assert_eq!(a.resource_id.as_deref(), Some("30050"));

        let b = AuditEntry::blocked(pid, "command_blocked", "command")
            .with_details(serde_json::json!({"reason": "db_access_nexus"}));
        assert_eq!(b.outcome, "blocked");
        assert_eq!(b.details["reason"], "db_access_nexus");

        let k = AuditEntry::killed(pid, "port_violation_kill", "port");
        assert_eq!(k.outcome, "killed");
        assert_eq!(k.actor, "system");
    }

    #[test]
    fn record_without_writer_does_not_panic() {
        // Senza chiamare start_writer, record deve degradare silenziosamente
        let pid = Uuid::new_v4();
        record_audit(AuditEntry::allowed(pid, "test", "port"));
    }
}
