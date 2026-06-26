//! Adapter del trait [`nexus_agent_graph::runtime::ports::ModelUpscalePort`].
//!
//! IMPLEMENTERA' (FASE 2):
//! - `context_window` (lookup del context window del modello corrente da
//!   `ai_price_catalog`; `0` se ignoto, fail-open su errore);
//! - `select_upscale_model` (selezione dinamica dal catalog di un modello con
//!   `context_window >= required_tokens` nel tier configurato, capable per tool use,
//!   escluso `agentic_thinking_policy = 'exclude'`, col provider risolto).
//! Tier-based e DB-driven (regola G): nessun nome modello hardcoded; tier e flag
//! (`agent.upscale.*`) sono settings letti via `sqlx`. CONFINE (regola L): la
//! DECISIONE di SE fare upscale resta PURA in
//! `nexus_agent_graph::decisions::end_turn`; qui SOLO l'I/O. BEST-EFFORT:
//! `Ok(0)` / `Ok(None)` su guasto, mai `PortError`. SOLA LETTURA: nessun gate `mode`.

use sqlx::PgPool;

/// Adapter [`ModelUpscalePort`] -> `ai_price_catalog` + settings `agent.upscale.*`.
///
/// F2 implementera' il trait `ModelUpscalePort` su questa struct.
pub struct CatalogModelUpscalePort {
    /// Pool Postgres su cui i lookup catalog/settings dell'upscale gireranno in F2.
    db: PgPool,
}

impl CatalogModelUpscalePort {
    /// Costruisce l'adapter sul pool Postgres condiviso.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}
