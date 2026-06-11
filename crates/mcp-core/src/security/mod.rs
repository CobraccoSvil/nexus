//! Modulo `security` — guardrail per l'accesso dei progetti alle risorse di sistema.
//!
//! Composto da:
//! - `quotas`  — quote per-progetto (porte, RAM, disk, container, pool DB) lette da
//!               `nexus_resource_quotas`. Cache 60s. Sentinella `00000000-...` = default globali.
//! - `audit`   — writer batch async per `nexus_resource_audit`. Non blocca hot path:
//!               accumula in mpsc, flush ogni 100 eventi o 5s. Esposto via `record(...)`.
//! - `port_enforcer` — task Tokio in background (cron 5s): scansiona `ss -tlnp`, killa
//!                     PID di progetto che bindano porta fuori dal bucket assegnato.
//!
//! - `resource_governance` — punto unico di governance risorse (mig 0397, regola L):
//!                           catalogo policy DB-driven, dispatcher enforcement in
//!                           scrittura (porte/URL), registrazione violazioni come
//!                           diagnosi `policy_violation` (pannello Problemi).
//!
//! Tutti i moduli convertono violazioni in `nexus_resource_audit` + (per il port_enforcer)
//! eventi `ProjectEvent::Notification` sul dispatcher per notification UI in real-time.

pub mod api;
pub mod audit;
pub mod guardrail_metrics;
pub mod port_enforcer;
pub mod quotas;
pub mod resource_governance;
pub mod resource_linter;

// Re-export simbolico per uso esterno
pub use audit::{record_audit, AuditEntry};
pub use quotas::{load_quota, ResourceQuota};
