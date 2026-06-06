//! Concrete learning workers
//!
//! Implementazioni dei worker rappresentativi del piano Ruflo.
//! Ogni worker è indipendente e registrabile singolarmente.
//!
//! ## Worker disponibili (12 totale)
//!
//! Reactive (OnTaskComplete):
//! - `UltralearnWorker`  — estrae pattern da task results
//! - `AuditWorker`       — scanning sicurezza post-task
//! - `MetricsAggregationWorker` — aggrega metriche batch
//! - `VersioningWorker`  — snapshot versioni Q-table/pattern
//!
//! Periodic:
//! - `CleanupWorker`          — evict entry scadute dal namespace
//! - `MemoryConsolidationWorker` — consolida semantica simile
//! - `ProfilingWorker`        — raccoglie metriche performance
//! - `AnomalyDetectionWorker` — rileva comportamenti anomali
//! - `SessionPersistenceWorker` — salva snapshot sessione
//! - `QLearningReplayWorker`  — replay esperienze per Q-Learning
//! - `ReplicationWorker`      — prepara batch per replica PostgreSQL
//! - `ClusteringWorker`       — raggruppa pattern per agent_type
//! - `PromptOptimizerWorker`  — varianti A/B prompt da metriche reflection
//! - `GuidelineAlignmentWorker` — conformance prompt vs direttive + revisioni
//!
//! Modulo condiviso:
//! - `prompt_variants` — safelist, insert variante+esperimento, client brain
//!   `/agent/prompt-revise` (riusato da optimizer e alignment, regola L)

pub mod anomaly;
pub mod audit;
pub mod cleanup;
pub mod clustering;
pub mod guideline_alignment;
pub mod memory_consolidation;
pub mod metrics;
pub mod profiling;
pub mod prompt_optimizer;
pub mod prompt_variants;
pub mod q_learning_replay;
pub mod replication;
pub mod session_persistence;
pub mod ultralearn;
pub mod versioning;

pub use anomaly::AnomalyDetectionWorker;
pub use audit::AuditWorker;
pub use cleanup::CleanupWorker;
pub use clustering::ClusteringWorker;
pub use guideline_alignment::GuidelineAlignmentWorker;
pub use memory_consolidation::MemoryConsolidationWorker;
pub use metrics::MetricsAggregationWorker;
pub use profiling::ProfilingWorker;
pub use prompt_optimizer::PromptOptimizerWorker;
pub use q_learning_replay::QLearningReplayWorker;
pub use replication::{ReplicationBatch, ReplicationEntry, ReplicationWorker};
pub use session_persistence::SessionPersistenceWorker;
pub use ultralearn::UltralearnWorker;
pub use versioning::VersioningWorker;
