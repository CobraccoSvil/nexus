//! Adapter del trait [`nexus_agent_graph::runtime::ports::VerifierRunStore`].
//!
//! IMPLEMENTERA' (FASE 2) `VerifierRunStore::record` con una INSERT best-effort su
//! `nexus_agent_verifier_runs` via `sqlx` (1:1 con
//! `verifier_node._persist_verifier_run`). La scrittura e' gata `Real` (no-op in
//! `ExecMode::Replay`, punto unico del gate shadow). Best-effort: su errore DB
//! l'impl logga e ritorna `Ok(())` (il `PortError` resta per un contratto rotto).

use sqlx::PgPool;

/// Adapter [`VerifierRunStore`] -> `nexus_agent_verifier_runs` via `sqlx`.
///
/// F2 implementera' il trait `VerifierRunStore` su questa struct.
pub struct PgVerifierRunStore {
    /// Pool Postgres su cui la INSERT del verifier girera' in F2.
    db: PgPool,
}

impl PgVerifierRunStore {
    /// Costruisce lo store sul pool Postgres condiviso.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}
