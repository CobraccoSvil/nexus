//! Adapter del trait [`nexus_agent_graph::runtime::ports::EscalationPort`].
//!
//! IMPLEMENTERA' (FASE 2) `EscalationPort::escalation_inputs` risolvendo gli input
//! dell'auto-escalation: lettura di `nexus_model_escalation_chain` (mig 0128) via
//! `sqlx`, consultazione del gate cooldown (ADR 0020, fonte unica) e risoluzione del
//! purpose `loop_fallback_default` dalla routing matrix (regola G). FAIL-OPEN: su
//! guasto di lettura ritorna `EscalationInputs` vuoto (la selezione risolve a `None`,
//! chiusura secca), mai un `PortError`. CONFINE (regola L): qui SOLO l'I/O; la
//! SELEZIONE resta nel modulo puro `nexus_agent_graph::decisions::escalation`.

use sqlx::PgPool;

/// Adapter [`EscalationPort`] -> `nexus_model_escalation_chain` + gate cooldown +
/// routing matrix.
///
/// F2 implementera' il trait `EscalationPort` su questa struct.
pub struct PgEscalationPort {
    /// Pool Postgres su cui la lettura della catena di escalation girera' in F2;
    /// affianchera' il gate cooldown ADR 0020 e la routing matrix.
    db: PgPool,
}

impl PgEscalationPort {
    /// Costruisce l'adapter sul pool Postgres condiviso.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}
