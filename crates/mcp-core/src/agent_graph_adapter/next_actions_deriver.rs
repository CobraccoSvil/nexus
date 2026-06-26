//! Adapter del trait [`nexus_agent_graph::runtime::ports::NextActionsDeriver`].
//!
//! IMPLEMENTERA' (FASE 2) `NextActionsDeriver::derive` derivando le scelte di
//! proseguimento dal testo dell'assistente (parita' con `next_actions.derive`):
//! parse del blocco machine-readable -> fallback deterministico "Prossimi passi"
//! -> fallback LLM sul purpose `choices_extractor` (risolto dalla routing matrix,
//! regola G). BEST-EFFORT: qualunque errore -> `Ok(vec![])`, mai `PortError`. La
//! RIMOZIONE del blocco `<suggested_actions>` resta PURA fuori da qui (regola L,
//! `decisions::end_turn::strip_suggested_actions`). Il pool risolve il purpose; il
//! gateway per il fallback LLM verra' affiancato in F2.

use sqlx::PgPool;

/// Adapter [`NextActionsDeriver`] -> parse + fallback LLM (`choices_extractor`).
///
/// F2 implementera' il trait `NextActionsDeriver` su questa struct.
pub struct NextActionsDeriverAdapter {
    /// Pool Postgres per la risoluzione del purpose `choices_extractor`; F2
    /// affianchera' il gateway per il fallback LLM.
    db: PgPool,
}

impl NextActionsDeriverAdapter {
    /// Costruisce l'adapter sul pool Postgres condiviso.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}
