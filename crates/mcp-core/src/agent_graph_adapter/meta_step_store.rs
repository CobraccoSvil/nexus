//! Adapter del trait [`nexus_agent_graph::runtime::ports::MetaStepStore`].
//!
//! IMPLEMENTERA' (FASE 2) `MetaStepStore::persist_meta_step` con una INSERT su
//! `agent_meta_steps` via `sqlx` (plan/routing/clarify/fallback/reflection
//! persistiti per la cronologia, distinti dal canale live SSE
//! [`super::event_sink`]). Gata `Real` (no-op in `ExecMode::Replay`). Best-effort:
//! errore DB loggato, `Ok(())`. E' un trait SEPARATO da `EventSink` (persistenza
//! async/fallibile/gata vs canale live sincrono/infallibile, vedi doc del trait).

use sqlx::PgPool;

/// Adapter [`MetaStepStore`] -> `agent_meta_steps` via `sqlx`.
///
/// F2 implementera' il trait `MetaStepStore` su questa struct.
pub struct PgMetaStepStore {
    /// Pool Postgres su cui la INSERT dei meta-step girera' in F2.
    db: PgPool,
}

impl PgMetaStepStore {
    /// Costruisce lo store sul pool Postgres condiviso.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}
