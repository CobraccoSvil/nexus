//! Adapter del trait [`nexus_agent_graph::runtime::ports::AgentStepStore`].
//!
//! IMPLEMENTERA' (FASE 2) `AgentStepStore::persist_step` con una INSERT su
//! `agent_steps` via `sqlx`. `step_index` deterministico = `iteration * 1000 + idx`;
//! l'impl DEVE usare `ON CONFLICT DO NOTHING` (idempotente sui retry) + guard
//! `untracked_run` (evita FK orfane). Gata `Real` (no-op in `ExecMode::Replay`,
//! punto unico del gate shadow). Best-effort: errore DB loggato, `Ok(())` ritornato.

use sqlx::PgPool;

/// Adapter [`AgentStepStore`] -> `agent_steps` via `sqlx`.
///
/// F2 implementera' il trait `AgentStepStore` su questa struct.
pub struct PgAgentStepStore {
    /// Pool Postgres su cui la INSERT degli step girera' in F2.
    db: PgPool,
}

impl PgAgentStepStore {
    /// Costruisce lo store sul pool Postgres condiviso.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}
