//! Errore tipizzato della risoluzione purpose→modello (regola M): vive qui,
//! nel crate condiviso, cosi' che ENTRAMBI i lati di un confine di crate
//! (chi risolve in mcp-core e chi consuma via port, es. nexus-wiki) parlino
//! lo stesso vocabolario (regola L). Chi deve DECIDERE sull'esito usa
//! [`PurposeUnresolved::in_chain`] o fa match sulla variante — mai parsing
//! del testo del messaggio: il `Display` (thiserror) esiste solo per log.

/// Esito negativo della risoluzione di un purpose model. Prodotto dal punto
/// unico `internal_routing::PurposeResolution::try_model` (mcp-core) e
/// propagato tipizzato lungo le catene `anyhow`.
#[derive(Debug, Clone, thiserror::Error)]
pub enum PurposeUnresolved {
    /// Tier configurato ma nessun modello disponibile (capability o cooldown).
    #[error(
        "nessun modello del tier '{tier}' disponibile per purpose '{purpose}' \
         (capability mancante o provider in cooldown)"
    )]
    NoCapableModel { purpose: String, tier: String },
    /// Purpose assente da `nexus_purpose_model` o privo di tier.
    #[error("purpose '{purpose}' non configurato o privo di tier in nexus_purpose_model")]
    NotFound { purpose: String },
    /// Routing matrix non disponibile (DB down): nessun fallback hardcoded.
    #[error("routing non disponibile per '{purpose}': {message}")]
    MatrixUnavailable { purpose: String, message: String },
}

impl PurposeUnresolved {
    /// Vero se la catena di un `anyhow::Error` contiene una risoluzione purpose
    /// fallita. E' il punto unico di DECISIONE (regola M): downcast sul tipo,
    /// mai `contains(...)` sul messaggio. Attraversa i `.context(...)`.
    pub fn in_chain(e: &anyhow::Error) -> bool {
        e.downcast_ref::<Self>().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_chain_riconosce_il_tipo_anche_sotto_context() {
        let e = anyhow::Error::from(PurposeUnresolved::NotFound {
            purpose: "wiki_title_gen".into(),
        });
        assert!(PurposeUnresolved::in_chain(&e));
        // Il downcast attraversa i .context() aggiunti a monte.
        let wrapped = e.context("batch fallito");
        assert!(PurposeUnresolved::in_chain(&wrapped));
    }

    #[test]
    fn in_chain_ignora_il_testo_del_messaggio() {
        // Mutazione contro la vecchia classificazione testuale: un errore
        // QUALUNQUE che cita "purpose non configurato" nel messaggio NON e'
        // una risoluzione fallita (regola M).
        let e = anyhow::anyhow!("template vuoto: purpose non configurato altrove");
        assert!(!PurposeUnresolved::in_chain(&e));
    }
}
