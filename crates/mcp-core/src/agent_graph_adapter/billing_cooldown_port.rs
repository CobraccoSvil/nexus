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
//! lettura dello snapshot. SOLA LETTURA.
//!
//! Stateless (delega a funzioni libere del modulo cooldown a stato globale):
//! l'adapter e' una unit-struct senza handle.

use async_trait::async_trait;
use nexus_agent_graph::runtime::ports::{BillingCooldownPort, PortError};

/// Adapter [`BillingCooldownPort`] -> [`crate::provider_cooldown`] (snapshot
/// cooldown billing, fonte unica).
#[derive(Default)]
pub struct CooldownBillingPort;

impl CooldownBillingPort {
    /// Costruisce l'adapter (stateless: delega alle funzioni del modulo cooldown).
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl BillingCooldownPort for CooldownBillingPort {
    /// Delega alla fonte unica del cooldown di mcp-core
    /// ([`crate::provider_cooldown::cooldown_snapshot`]): la STESSA lista che il
    /// selettore di modello e l'handler `/api/internal/routing/cooldown` usano per
    /// saltare i provider non disponibili (regola L, niente snapshot duplicato).
    ///
    /// Nomi in lowercase + ordinati alfabeticamente (parita' col Python
    /// `sorted(snap.keys())`, cosi' il messaggio di fail-fast e' deterministico).
    /// FAIL-OPEN: `cooldown_snapshot()` non e' fallibile (ritorna `Vec` vuoto se la
    /// mappa non e' ancora inizializzata o il lock e' avvelenato), quindi qui non
    /// c'e' mai un `PortError` nel flusso normale.
    async fn billing_exhausted_providers(&self) -> Result<Vec<String>, PortError> {
        let mut providers: Vec<String> = crate::provider_cooldown::cooldown_snapshot()
            .into_iter()
            .map(|(name, _secs, _reason)| name.to_lowercase())
            .collect();
        providers.sort();
        providers.dedup();
        Ok(providers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fail-open + invariante d'ordine: la funzione ritorna sempre `Ok` con una
    /// lista ordinata alfabeticamente e senza duplicati. Non inquina lo stato
    /// globale del cooldown (idempotente, regola F): asserisce solo le proprieta'
    /// strutturali, indipendenti dal contenuto effettivo della mappa cooldown.
    #[tokio::test]
    async fn billing_exhausted_e_sempre_ordinato_e_fail_open() {
        let port = CooldownBillingPort::new();
        let providers = port
            .billing_exhausted_providers()
            .await
            .expect("fail-open: mai un PortError nel flusso normale");
        // Ordinato alfabeticamente (parita' messaggio fail-fast Python).
        let mut sorted = providers.clone();
        sorted.sort();
        assert_eq!(providers, sorted, "la lista deve essere ordinata");
        // Senza duplicati e tutto lowercase.
        let mut deduped = providers.clone();
        deduped.dedup();
        assert_eq!(providers, deduped, "niente duplicati");
        assert!(
            providers.iter().all(|p| p == &p.to_lowercase()),
            "i nomi provider devono essere lowercase"
        );
    }
}
