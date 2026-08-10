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

use crate::provider_readiness::ModelFact;

/// Copertura della dichiarazione di un fornitore. Chiuso: ogni variante ha un
/// rimedio diverso, ed e' per questo che sono varianti e non un conteggio nudo
/// (regola Q).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclarationCoverage {
    /// Nessun modello ABILITATO: non c'e' nulla da dichiarare, e una riga di
    /// capability per un modello che nessuno instradera' non servirebbe a nulla.
    /// Non e' un difetto e non chiede interventi.
    NothingToDeclare,
    /// Ogni modello abilitato ha la sua riga: i consumatori leggono cio' che e'
    /// stato dichiarato per quel modello, non un default di famiglia.
    Complete { models: usize },
    /// Alcuni modelli abilitati sono scoperti. E' la forma che prende il
    /// catalogo VIVO: la migrazione del fornitore c'e', il discovery ha aggiunto
    /// modelli dopo di lei. Il rimedio e' per-modello e ricorrente.
    Partial { declared: usize, undeclared: usize },
    /// NESSUN modello abilitato ha una riga: il fornitore e' interamente fuori
    /// dalla vista capability. E' la forma che prende l'ONBOARDING senza la sua
    /// migrazione, e il rimedio e' un atto solo.
    Absent { undeclared: usize },
}

impl DeclarationCoverage {
    /// Identificatore canonico sul wire (regola N).
    pub fn wire(&self) -> &'static str {
        match self {
            DeclarationCoverage::NothingToDeclare => "nothing_to_declare",
            DeclarationCoverage::Complete { .. } => "complete",
            DeclarationCoverage::Partial { .. } => "partial",
            DeclarationCoverage::Absent { .. } => "absent",
        }
    }

    /// Quanti modelli abilitati sono scoperti. `0` quando non ne manca nessuno:
    /// qui lo zero e' una misura, non un'assenza di misura.
    pub fn undeclared(&self) -> usize {
        match self {
            DeclarationCoverage::NothingToDeclare | DeclarationCoverage::Complete { .. } => 0,
            DeclarationCoverage::Partial { undeclared, .. }
            | DeclarationCoverage::Absent { undeclared } => *undeclared,
        }
    }

    /// `true` quando la copertura pretende un intervento umano. Nessun ciclo a
    /// runtime scrive capability (vedi il doc del modulo): aspettare non cambia
    /// nulla. E' il campo su cui un allarme decide, mai una stringa.
    pub fn richiede_intervento(&self) -> bool {
        matches!(
            self,
            DeclarationCoverage::Partial { .. } | DeclarationCoverage::Absent { .. }
        )
    }
}

/// Classifica la copertura dai fatti di catalogo. PURA: nessun I/O.
///
/// Contano SOLO i modelli abilitati. Un modello disabilitato non viene mai
/// instradato, quindi la sua capability mancante non e' un difetto e contarla
/// produrrebbe un allarme che nessun intervento puo' spegnere — `ai_price_catalog`
/// porta 528 righe contro le 128 abilitate, e quasi tutte le disabilitate sono
/// modelli storici che nessuno dichiarera' mai.
pub fn classifica_dichiarazione(models: &[ModelFact]) -> DeclarationCoverage {
    let mut declared = 0usize;
    let mut undeclared = 0usize;
    for m in models.iter().filter(|m| m.is_enabled) {
        if m.ha_capability {
            declared += 1;
        } else {
            undeclared += 1;
        }
    }
    match (declared, undeclared) {
        (0, 0) => DeclarationCoverage::NothingToDeclare,
        (_, 0) => DeclarationCoverage::Complete { models: declared },
        (0, undeclared) => DeclarationCoverage::Absent { undeclared },
        (declared, undeclared) => DeclarationCoverage::Partial {
            declared,
            undeclared,
        },
    }
}

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
    use crate::provider_readiness::{classifica, ProviderReadiness};

    fn modello(is_enabled: bool, ha_capability: bool) -> ModelFact {
        ModelFact {
            is_enabled,
            capability_source: "auto".to_string(),
            auto_disabled_reason: None,
            ha_capability,
        }
    }

    #[test]
    fn openrouter_e_assente_non_parziale() {
        // Lo stato REALE del 10/08/2026: 17 modelli abilitati, 17 scoperti.
        let modelli = vec![modello(true, false); 17];
        let c = classifica_dichiarazione(&modelli);
        assert_eq!(c, DeclarationCoverage::Absent { undeclared: 17 });
        assert!(c.richiede_intervento());
    }

    #[test]
    fn openai_e_parziale_non_assente() {
        // 65 abilitati, 11 scoperti: la migrazione c'e', il discovery ha
        // aggiunto modelli dopo. Rimedio diverso da quello di openrouter, e per
        // questo sono due varianti.
        let mut modelli = vec![modello(true, true); 54];
        modelli.extend(vec![modello(true, false); 11]);
        let c = classifica_dichiarazione(&modelli);
        assert_eq!(
            c,
            DeclarationCoverage::Partial {
                declared: 54,
                undeclared: 11
            }
        );
        assert!(c.richiede_intervento());
        assert_ne!(c.wire(), DeclarationCoverage::Absent { undeclared: 11 }.wire());
    }

    #[test]
    fn solo_i_modelli_abilitati_contano() {
        // MUTAZIONE (regola O): se il criterio guardasse tutte le righe di
        // catalogo, questo fornitore risulterebbe scoperto — e nessun intervento
        // potrebbe spegnere l'allarme, perche' quei modelli non si dichiarano.
        let modelli = vec![
            modello(true, true),
            modello(false, false),
            modello(false, false),
        ];
        assert_eq!(
            classifica_dichiarazione(&modelli),
            DeclarationCoverage::Complete { models: 1 },
            "un modello disabilitato non viene instradato: la sua capability \
             mancante non e' un difetto"
        );
    }

    #[test]
    fn nessun_modello_abilitato_non_e_un_difetto() {
        // Distinto da `Absent`: li' ci sono modelli instradabili e scoperti, qui
        // non c'e' nulla da dichiarare.
        let c = classifica_dichiarazione(&[modello(false, false)]);
        assert_eq!(c, DeclarationCoverage::NothingToDeclare);
        assert!(!c.richiede_intervento());
        assert_eq!(classifica_dichiarazione(&[]), DeclarationCoverage::NothingToDeclare);
    }

    #[test]
    fn la_copertura_completa_non_chiede_interventi() {
        let c = classifica_dichiarazione(&vec![modello(true, true); 4]);
        assert_eq!(c, DeclarationCoverage::Complete { models: 4 });
        assert!(!c.richiede_intervento());
        assert_eq!(c.undeclared(), 0);
    }

    #[test]
    fn la_dichiarazione_non_e_la_prontezza() {
        // IL TEST CHE GIUSTIFICA IL MODULO. Lo stato reale di groq: sano — 2257
        // osservazioni in `nexus_provider_health_history`, l'ultima di oggi — e
        // interamente scoperto. Se la copertura fosse una `CausaStallo`, questo
        // caso non potrebbe esistere: `classifica` ritorna `Observed` e la
        // variante di stallo sarebbe irraggiungibile proprio qui.
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
        scrivi_dichiarazione(&mut p, &classifica_dichiarazione(&vec![modello(true, false); 17]));
        assert_eq!(p["declaration"], "absent");
        assert_eq!(p["declaration_undeclared"], 17);

        // Coperto per intero: nessun conteggio, o sarebbe uno zero che invita a
        // cercare qualcosa che non c'e'.
        let mut q = serde_json::json!({ "name": "kimi" });
        scrivi_dichiarazione(&mut q, &classifica_dichiarazione(&vec![modello(true, true); 4]));
        assert_eq!(q["declaration"], "complete");
        assert!(q.get("declaration_undeclared").is_none());
    }

    /// I fatti li porta `provider_readiness::carica_fatti_catalogo`, che
    /// interroga la VISTA `v_model_capabilities` — la stessa che i consumatori
    /// leggono a runtime (regola O). Chiedere alla tabella sarebbe una seconda
    /// idea di «dichiarato», e divergerebbe il giorno in cui la vista cambia.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_copertura_si_misura_sulla_vista_che_la_produzione_legge(pool: sqlx::PgPool) {
        // Due modelli a catalogo. Il trigger del gate 0629 li respinge a
        // `is_enabled=false`: si abilitano dando loro la prova che quel gate
        // pretende, invece di seminare lo stato finale a mano.
        for i in 0..2 {
            sqlx::query(
                "INSERT INTO ai_price_catalog \
                    (provider, model, display_name, input_cost_per_million_tokens, \
                     output_cost_per_million_tokens, currency, is_enabled, capability_source) \
                 VALUES ('zeta', $1, $1, 1.0, 1.0, 'USD', true, 'auto')",
            )
            .bind(format!("zeta-modello-{i}"))
            .execute(&pool)
            .await
            .expect("seed catalog");
        }
        sqlx::query(
            "UPDATE ai_price_catalog \
                SET is_enabled = true, last_probe_healthy_at = NOW(), \
                    auto_disabled_reason = NULL, auto_disabled_at = NULL \
              WHERE provider = 'zeta'",
        )
        .execute(&pool)
        .await
        .expect("abilita con la prova che il gate pretende");

        // UNO dei due riceve la sua riga di capability.
        sqlx::query(
            "INSERT INTO nexus_provider_capabilities (provider, model) \
             VALUES ('zeta', 'zeta-modello-0')",
        )
        .execute(&pool)
        .await
        .expect("seed capability");

        // PREMESSA misurata: la vista vede uno solo dei due.
        let nella_vista: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM v_model_capabilities WHERE provider = 'zeta'",
        )
        .fetch_one(&pool)
        .await
        .expect("conteggio vista");
        assert_eq!(
            nella_vista, 1,
            "la vista parte da nexus_provider_capabilities: un modello senza riga \
             non vi compare affatto"
        );

        let fatti = crate::provider_readiness::carica_fatti_catalogo(&pool).await;
        let modelli = fatti.get("zeta").cloned().unwrap_or_default();
        assert_eq!(
            classifica_dichiarazione(&modelli),
            DeclarationCoverage::Partial {
                declared: 1,
                undeclared: 1
            }
        );

        // MUTAZIONE (regola O): si dichiara anche il secondo. La copertura DEVE
        // ribaltarsi — se restasse `Partial`, il caricamento non starebbe
        // guardando la vista.
        sqlx::query(
            "INSERT INTO nexus_provider_capabilities (provider, model) \
             VALUES ('zeta', 'zeta-modello-1')",
        )
        .execute(&pool)
        .await
        .expect("seed capability 2");
        let fatti = crate::provider_readiness::carica_fatti_catalogo(&pool).await;
        let modelli = fatti.get("zeta").cloned().unwrap_or_default();
        let c = classifica_dichiarazione(&modelli);
        assert_eq!(c, DeclarationCoverage::Complete { models: 2 });
        assert!(!c.richiede_intervento());
    }
}
