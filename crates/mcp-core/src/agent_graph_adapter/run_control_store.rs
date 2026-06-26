//! Adapter del trait [`nexus_agent_graph::runtime::ports::RunControlStore`].
//!
//! IMPLEMENTERA' (FASE 2) il controllo di run condiviso da `executor` e
//! `tool_dispatch` (PUNTO UNICO, regola L) su `agent_runs` via `sqlx`:
//! - `is_superseded` (lettura del flag `superseded`/`supersede_active_runs`,
//!   FAIL-OPEN: errore DB -> `Ok(false)`, il run prosegue);
//! - `heartbeat` (UPDATE `updated_at` best-effort, gata `Real`);
//! - `set_effective_model` (registra provider/model effettivi dal gateway, gata
//!   `Real`).
//! Le scritture sono no-op in `ExecMode::Replay` (il run shadow non tocca la
//! telemetria del primario).

use sqlx::PgPool;

/// Adapter [`RunControlStore`] -> `agent_runs` via `sqlx`.
///
/// F2 implementera' il trait `RunControlStore` su questa struct.
pub struct PgRunControlStore {
    /// Pool Postgres su cui le letture/UPDATE del controllo run gireranno in F2.
    db: PgPool,
}

impl PgRunControlStore {
    /// Costruisce lo store sul pool Postgres condiviso.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}
