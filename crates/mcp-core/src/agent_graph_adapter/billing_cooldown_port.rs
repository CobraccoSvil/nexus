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
    /// ([`crate::provider_cooldown::fornitori_in_cooldown`]): la STESSA lista che
    /// il selettore di modello usa per saltare i FORNITORI non disponibili
    /// (regola L, niente snapshot duplicato).
    ///
    /// PORTATA: solo i fornitori esclusi PER INTERO. Il consumatore e'
    /// `billing_fail_fast_message`, che dichiara «i provider sono esauriti» e
    /// ferma il run: un tetto su un singolo modello non e' un fornitore esaurito,
    /// e contarlo qui fermerebbe un run che poteva girare sugli altri modelli
    /// dello stesso fornitore. Prima quella lista portava le CHIAVI grezze del
    /// cooldown, quindi il messaggio di fail-fast avrebbe nominato
    /// `groq\u{1}openai/gpt-oss-20b` come se fosse un fornitore.
    ///
    /// Nomi in lowercase + ordinati alfabeticamente (parita' col Python
    /// `sorted(snap.keys())`, cosi' il messaggio di fail-fast e' deterministico).
    /// FAIL-OPEN: la lettura non e' fallibile (ritorna `Vec` vuoto se la mappa non
    /// e' ancora inizializzata o il lock e' avvelenato), quindi qui non c'e' mai
    /// un `PortError` nel flusso normale.
    async fn billing_exhausted_providers(&self) -> Result<Vec<String>, PortError> {
        Ok(crate::provider_cooldown::fornitori_in_cooldown())
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
