//! «I modelli di questo fornitore possono essere SCELTI — e se nessuno puo',
//! qualcuno lo sta ancora misurando?»
//!
//! TERZA domanda della famiglia, sugli stessi fatti di catalogo delle altre
//! due, e ortogonale a entrambe:
//!
//!   - [`crate::provider_readiness`] chiede se SAPPIAMO che il fornitore
//!     risponde;
//!   - [`crate::provider_declaration`] chiede se cio' che sappiamo BASTA a
//!     usarlo;
//!   - questa chiede se il gate di qualificazione lo AMMETTE.
//!
//! Le tre non si possono fondere, e il caso che lo prova e' reale: al
//! 20/08/2026 `groq` e' insieme sano (53 chiamate finalizzate in tre giorni),
//! interamente scoperto di capability, e strutturalmente fuori dal routing per
//! intent. Tre campi, tre rimedi.
//!
//! Il CRITERIO e i FATTI vivono nel crate `nexus-capability-audit` (regola L),
//! dove li raggiunge anche `xtask capability-census`: mcp-core e' bin-only,
//! quindi l'alternativa non era «xtask chiama mcp-core», era «xtask ricopia il
//! criterio». Qui resta la sola resa sul wire, che e' di mcp-core.
//!
//! Per il difetto misurato che ha fatto nascere il criterio — 14 righe di
//! routing groq spente da 36 giorni di giri che non misurano nulla — vedi la
//! doc del modulo `nexus_capability_audit::selezionabilita`.

use nexus_capability_audit::ProviderSelectability;

pub use nexus_capability_audit::classifica_selezionabilita;

/// Scrive la selezionabilita' sull'entry JSON di un fornitore, accanto alla
/// prontezza e alla dichiarazione. Unico compositore (regola L), come
/// `provider_readiness::scrivi_prontezza`.
///
/// Il testo NON si compone qui (regola Q, punto 3): il wire porta i campi e la
/// UI ne fa una frase nella lingua dell'utente.
pub fn scrivi_selezionabilita(p: &mut serde_json::Value, s: &ProviderSelectability) {
    p["selectability"] = serde_json::json!(s.wire());
    // `routable` e' la lettura secca su cui un pannello accende o spegne una
    // riga: senza, ogni consumatore dovrebbe ricostruirla dal vocabolario, e
    // sarebbe una seconda idea della stessa domanda.
    p["selectable_for_routing"] = serde_json::json!(s.instradabile());
    // Il conteggio si scrive solo dove ha un significato: altrove sarebbe uno
    // zero che invita a cercare qualcosa che non c'e'.
    if s.richiede_intervento() {
        p["selectability_stuck_models"] = serde_json::json!(s.stuck());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_readiness::{classifica, ModelFact, ProviderReadiness};

    fn groq_reale() -> Vec<ModelFact> {
        // Lo stato REALE di groq il 20/08/2026: tre modelli abilitati, nessuno
        // qualificato, due fermi su `round_not_measuring:provider_saturated`.
        let stuck = ModelFact {
            is_enabled: true,
            capability_source: "manual".to_string(),
            auto_disabled_reason: None,
            ha_capability: false,
            qualification_valid: false,
            qualification_attempts: 4,
            qualification_reason: Some("round_not_measuring:provider_saturated".to_string()),
            qualification_state: "unqualified".to_string(),
        };
        let squalificato = ModelFact {
            qualification_attempts: 1,
            qualification_reason: Some("tool_smoke:error_class:invalid_request".to_string()),
            qualification_state: "disqualified".to_string(),
            ..stuck.clone()
        };
        vec![stuck.clone(), stuck, squalificato]
    }

    // Il criterio in se' e' provato nel crate `nexus-capability-audit`, dove
    // vive. Qui restano le prove che riguardano mcp-core: che le tre domande
    // non si confondano, e che il wire non perda il dettaglio.

    /// IL TEST CHE GIUSTIFICA LA SEPARAZIONE, e non e' teorico: e' groq il
    /// 20/08/2026. Sano — il probe di salute risponde — e insieme fuori dal
    /// routing. Se la selezionabilita' fosse una `CausaStallo` di
    /// `provider_readiness`, questo caso non potrebbe esistere: `classifica`
    /// ritorna `Observed` appena una misura di salute c'e', e la variante nuova
    /// sarebbe irraggiungibile proprio qui.
    #[test]
    fn la_selezionabilita_non_e_la_prontezza() {
        let modelli = groq_reale();
        assert_eq!(
            classifica(true, &modelli, Some(true)),
            ProviderReadiness::Observed { healthy: true },
            "il fornitore E' sano: la prontezza dice il vero"
        );
        let s = classifica_selezionabilita(&modelli, true);
        assert!(
            !s.instradabile(),
            "e insieme il gate non ne ammette un solo modello"
        );
        assert!(s.richiede_intervento());
    }

    /// Il wire porta i tre campi senza che uno possa essere dedotto dagli
    /// altri: un fornitore sano puo' essere non instradabile, e un pannello che
    /// leggesse la sola prontezza lo mostrerebbe verde.
    #[test]
    fn il_wire_porta_la_selezionabilita_accanto_alle_altre_due() {
        let mut p = serde_json::json!({ "name": "groq", "healthy": true });
        scrivi_selezionabilita(&mut p, &classifica_selezionabilita(&groq_reale(), true));
        assert_eq!(p["selectability"], "stuck_unmeasured");
        assert_eq!(p["selectable_for_routing"], false);
        assert_eq!(
            p["selectability_stuck_models"], 2,
            "quanti modelli sono fermi e' cio' su cui si dimensiona l'intervento"
        );
        assert_eq!(p["healthy"], true, "la resa non tocca le altre risposte");
    }

    /// Dove non c'e' nulla da fare, il campo del conteggio non compare: uno
    /// zero inviterebbe a cercare modelli fermi che non esistono.
    #[test]
    fn il_conteggio_si_scrive_solo_dove_significa_qualcosa() {
        let sano = ModelFact {
            qualification_valid: true,
            qualification_state: "qualified".to_string(),
            qualification_attempts: 2,
            qualification_reason: None,
            ..groq_reale()[0].clone()
        };
        let mut p = serde_json::json!({ "name": "deepseek" });
        scrivi_selezionabilita(&mut p, &classifica_selezionabilita(&[sano], true));
        assert_eq!(p["selectability"], "selectable");
        assert_eq!(p["selectable_for_routing"], true);
        assert!(p.get("selectability_stuck_models").is_none());
    }
}
