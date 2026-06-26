//! Adapter del trait [`nexus_agent_graph::runtime::ports::BillingCooldownPort`].
//!
//! IMPLEMENTERA' (FASE 2) `BillingCooldownPort::billing_exhausted_providers`
//! delegando alla fonte unica del cooldown billing/quota di mcp-core
//! ([`crate::provider_cooldown`], snapshot in-memory + Redis; parita' con
//! `brain.providers.registry.get_billing_cooldown_snapshot`). La lista DEVE
//! arrivare GIA' ORDINATA alfabeticamente. FAIL-OPEN: un guasto di lettura ->
//! `Ok(vec![])` (nessun fail-fast, il run prosegue), mai un `PortError`. CONFINE
//! (regola L): la DECISIONE fail-fast resta PURA in
//! `nexus_agent_graph::decisions::end_turn::billing_fail_fast_message`; qui SOLO la
//! lettura dello snapshot. SOLA LETTURA: nessun gate `mode`.
//!
//! Stateless (delega a funzioni libere del modulo cooldown a stato globale):
//! l'adapter e' una unit-struct senza handle.

/// Adapter [`BillingCooldownPort`] -> [`crate::provider_cooldown`] (snapshot
/// cooldown billing, fonte unica).
///
/// F2 implementera' il trait `BillingCooldownPort` su questa struct.
#[derive(Default)]
pub struct CooldownBillingPort;

impl CooldownBillingPort {
    /// Costruisce l'adapter (stateless: delega alle funzioni del modulo cooldown).
    pub fn new() -> Self {
        Self
    }
}
