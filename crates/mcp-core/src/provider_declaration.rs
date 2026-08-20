//! «La DICHIARAZIONE di questo fornitore copre i modelli che il sistema puo'
//! instradare — e se non li copre, qualcuno la completera'?»
//!
//! PUNTO UNICO (regola L) della copertura di `nexus_provider_capabilities`, e
//! domanda ORTOGONALE a quella di [`crate::provider_readiness`]. Quella chiede
//! se SAPPIAMO che il fornitore risponde; questa chiede se cio' che sappiamo
//! BASTA a usarlo come il resto del sistema si aspetta. Un fornitore puo'
//! rispondere benissimo a ogni probe e non avere una sola riga di capability.
//!
//! PERCHE' NON E' UNA `CausaStallo`. E' la strada che sembra ovvia, e non
//! funziona: `provider_readiness::classifica` ritorna `Observed` appena esiste
//! una misura di salute, e l'osservazione precede per costruzione tutto cio' che
//! viene dopo — una variante di stallo nuova sarebbe IRRAGGIUNGIBILE proprio per
//! i fornitori che questo difetto riguarda. MISURATO il 10/08/2026 sul DB meta
//! vivo: `nexus_provider_health_history` ha 2257 righe per groq, 6223 per
//! openrouter, 4004 per perplexity, tutte con l'ultima osservazione dello stesso
//! giorno; e la fixture del wire (`__wire__/gateway-providers.json`, catturata
//! dall'endpoint reale) mostra `groq` con `readiness: "healthy"`. Conflagrare le
//! due domande avrebbe anche perso l'informazione vera — quel fornitore E' sano.
//!
//! MISURATO il 10/08/2026, `ai_price_catalog` incrociato con la vista: **37
//! modelli ABILITATI su 128 non hanno una riga di capability** —
//! openrouter 17 su 17, openai 11 su 65, perplexity 3 su 3, groq 2 su 2,
//! anthropic 2 su 9, google 2 su 9; deepseek, kimi e mistral coperti per intero.
//!
//! PERCHE' NESSUNO LA COMPLETA. Le scritture di `nexus_provider_capabilities`
//! vengono TUTTE da migrazioni (0240, 0318, 0319, 0478, 0556, 0690, 0694):
//! nel codice Rust l'unica `INSERT` su quella tabella sta dentro un
//! `#[sqlx::test]` di `native_engine`, cioe' e' un seed di test. Nessun ciclo a
//! runtime scopre un modello scoperto, ed e' la ragione per cui la condizione e'
//! `richiede_intervento()`: non si scioglie aspettando.
//!
//! PERCHE' NON BASTA UN GUARD DI BUILD. Undici dei 37 sono di `openai`, che ha
//! 62 righe di capability e la sua migrazione di onboarding da un pezzo: quei
//! modelli sono entrati nel catalogo dal discovery a runtime, DOPO il build. Un
//! guard testuale non puo' vederli perche' nascono dopo di lui — il difetto non
//! e' solo di onboarding, e' di catalogo vivo, quindi si misura a runtime.
//!
//! CONSEGUENZA di una riga mancante: i consumatori della vista ripiegano sui
//! default per FAMIGLIA (`capability::default_style_for_provider`). Per la
//! famiglia OpenAI-compatibile il ripiego e' plausibile; per un fornitore fuori
//! famiglia — `perplexity`, che quella mappa non nomina — e' `None`, cioe' il
//! force della tool call resta spento senza che nulla lo dichiari.

use nexus_capability_audit::DeclarationCoverage;

// Il CRITERIO e i FATTI vivono nel crate `nexus-capability-audit` (regola L):
// li' li raggiunge anche `xtask capability-census`, che senza quel crate
// dovrebbe ricopiarli — e una copia che diverge in silenzio e' il difetto che
// la regola O descrive. Qui resta la sola resa sul wire, che e' di mcp-core.
pub use nexus_capability_audit::classifica_dichiarazione;

/// Scrive la copertura sull'entry JSON di un fornitore, accanto alla prontezza.
/// Unico compositore (regola L), come `provider_readiness::scrivi_prontezza`.
///
/// Il testo NON si compone qui (regola Q, punto 3): il wire porta i campi e la
/// UI ne fa una frase nella lingua dell'utente.
pub fn scrivi_dichiarazione(p: &mut serde_json::Value, coverage: &DeclarationCoverage) {
    p["declaration"] = serde_json::json!(coverage.wire());
    // Il conteggio si scrive solo dove ha un significato: per le due varianti
    // complete sarebbe uno zero che invita a cercare qualcosa che non c'e'.
    if coverage.richiede_intervento() {
        p["declaration_undeclared"] = serde_json::json!(coverage.undeclared());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_readiness::{classifica, ModelFact, ProviderReadiness};

    fn modello(is_enabled: bool, ha_capability: bool) -> ModelFact {
        ModelFact {
            is_enabled,
            capability_source: "auto".to_string(),
            auto_disabled_reason: None,
            ha_capability,
            // La qualificazione e' la TERZA domanda sugli stessi fatti
            // (`nexus_capability_audit::selezionabilita`) e qui non decide
            // nulla: lo stato d'ingresso e' cio' che rende questi casi
            // confrontabili con quelli della copertura.
            qualification_valid: false,
            qualification_attempts: 0,
            qualification_reason: None,
            qualification_state: "unqualified".to_string(),
        }
    }

    // Il criterio in se' e' provato nel crate `nexus-capability-audit`, dove
    // vive. Qui restano le prove che riguardano mcp-core: che le due domande non
    // si confondano, e che il wire non perda il dettaglio.

    #[test]
    fn la_dichiarazione_non_e_la_prontezza() {
        // IL TEST CHE GIUSTIFICA LA SEPARAZIONE. Lo stato reale di groq: sano —
        // 2257 osservazioni in `nexus_provider_health_history`, l'ultima di oggi
        // — e interamente scoperto. Se la copertura fosse una `CausaStallo`,
        // questo caso non potrebbe esistere: `classifica` ritorna `Observed` e
        // la variante di stallo sarebbe irraggiungibile proprio qui.
        let modelli = vec![modello(true, false); 2];
        assert_eq!(
            classifica(true, &modelli, Some(true)),
            ProviderReadiness::Observed { healthy: true },
            "la salute e' misurata e resta misurata"
        );
        assert!(!classifica(true, &modelli, Some(true)).richiede_intervento());
        // ...e cio' nonostante la dichiarazione manca del tutto.
        let c = classifica_dichiarazione(&modelli);
        assert_eq!(c, DeclarationCoverage::Absent { undeclared: 2 });
        assert!(
            c.richiede_intervento(),
            "le due domande sono indipendenti: sano non implica dichiarato"
        );
    }

    #[test]
    fn la_prontezza_non_guarda_la_dichiarazione() {
        // MUTAZIONE dichiarata nel verso opposto: cambiare SOLO `ha_capability`
        // non deve spostare di un millimetro il verdetto di prontezza. Se un
        // giorno lo spostasse, le due domande si sarebbero confuse.
        for observed in [None, Some(true), Some(false)] {
            assert_eq!(
                classifica(true, &[modello(true, false)], observed),
                classifica(true, &[modello(true, true)], observed),
                "la capability non e' un fatto sulla salute"
            );
        }
    }

    #[test]
    fn il_wire_non_perde_il_conteggio() {
        let mut p = serde_json::json!({ "name": "openrouter" });
        scrivi_dichiarazione(
            &mut p,
            &classifica_dichiarazione(&vec![modello(true, false); 17]),
        );
        assert_eq!(p["declaration"], "absent");
        assert_eq!(p["declaration_undeclared"], 17);

        // Coperto per intero: nessun conteggio, o sarebbe uno zero che invita a
        // cercare qualcosa che non c'e'.
        let mut q = serde_json::json!({ "name": "kimi" });
        scrivi_dichiarazione(
            &mut q,
            &classifica_dichiarazione(&vec![modello(true, true); 4]),
        );
        assert_eq!(q["declaration"], "complete");
        assert!(q.get("declaration_undeclared").is_none());
    }
}
