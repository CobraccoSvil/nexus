//! «La DICHIARAZIONE di questo fornitore copre i modelli che il sistema puo'
//! instradare — e se non li copre, qualcuno la completera'?»
//!
//! Punto unico (regola L) della copertura di `nexus_provider_capabilities`.
//! Nasce in `mcp-core/src/provider_declaration.rs` (commit d787c257) e si
//! trasferisce qui INTERO, criterio e query, perche' il censimento a riga di
//! comando deve porre la domanda esattamente come la pone il pannello: mcp-core
//! e' bin-only, quindi l'alternativa non era «xtask chiama mcp-core», era «xtask
//! ricopia il criterio» — e due copie della stessa domanda che divergono in
//! silenzio sono il difetto che la regola O descrive. La resa sul wire resta in
//! mcp-core: il verdetto e' condiviso, la frase no.
//!
//! PERCHE' NON E' UNA `CausaStallo` di `provider_readiness`. E' la strada che
//! sembra ovvia e non funziona: `classifica` ritorna `Observed` appena esiste
//! una misura di salute, e l'osservazione precede per costruzione tutto cio' che
//! viene dopo — una variante di stallo nuova sarebbe IRRAGGIUNGIBILE proprio per
//! i fornitori che questo difetto riguarda. MISURATO il 10/08/2026 sul META
//! vivo: `nexus_provider_health_history` ha 2257 righe per groq, 6223 per
//! openrouter, 4004 per perplexity, tutte con l'ultima osservazione dello stesso
//! giorno. Quei fornitori SONO sani e sono scoperti: due domande, due campi.
//!
//! MISURATO il 10/08/2026, `ai_price_catalog` incrociato con la vista: **37
//! modelli ABILITATI su 128 non hanno una riga di capability** — openrouter 17
//! su 17, openai 11 su 65, perplexity 3 su 3, groq 2 su 2, anthropic 2 su 9,
//! google 2 su 9; deepseek, kimi e mistral coperti per intero.
//!
//! PERCHE' NESSUNO LA COMPLETA. Le scritture di `nexus_provider_capabilities`
//! vengono TUTTE da migrazioni (0240, 0318, 0319, 0478, 0556, 0690, 0694): nel
//! codice Rust l'unica `INSERT` su quella tabella sta dentro un `#[sqlx::test]`,
//! cioe' e' un seed. Nessun ciclo a runtime scopre un modello scoperto, ed e' la
//! ragione per cui la condizione e' `richiede_intervento()`: non si scioglie
//! aspettando.
//!
//! PERCHE' NON BASTA UN GUARD DI BUILD. Undici dei 37 sono di `openai`, che ha
//! 62 righe di capability e la sua migrazione di onboarding da un pezzo: quei
//! modelli sono entrati nel catalogo dal discovery a runtime, DOPO il build. Un
//! guard testuale non puo' vederli perche' nascono dopo di lui.

use std::collections::HashMap;

use sqlx::Row;

/// Fatto di UN modello a catalogo, nella forma minima su cui i cicli di verifica
/// e la copertura decidono. I nomi dei campi sono quelli delle colonne di
/// `ai_price_catalog`: non c'e' traduzione, quindi non c'e' dove sbagliarla.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFact {
    pub is_enabled: bool,
    pub capability_source: String,
    pub auto_disabled_reason: Option<String>,
    /// Il modello compare nella vista `v_model_capabilities`, cioe' la sua
    /// dichiarazione esiste. NON e' un fatto sulla salute: `provider_readiness`
    /// non lo guarda, e un test in mcp-core lo prova nel verso opposto.
    pub ha_capability: bool,
}

/// La query dei fatti. Costante ESPOSTA perche' e' la premessa di ogni numero
/// che ne discende: uno strumento che dichiara «37 scoperti» deve poter dire
/// anche COME l'ha chiesto (regola O, punto 4).
///
/// La copertura si chiede alla VISTA, non alla tabella che la alimenta: la vista
/// e' cio' che i consumatori interrogano a runtime, quindi «dichiarato» deve
/// significare qui esattamente cio' che significa li'. Interrogare la tabella
/// sarebbe una seconda idea dello stesso concetto, e divergerebbe il giorno in
/// cui la vista cambia definizione.
pub const SQL_FATTI_CATALOGO: &str = "SELECT c.provider, c.is_enabled, \
        COALESCE(c.capability_source, 'auto') AS capability_source, \
        c.auto_disabled_reason, \
        (v.model IS NOT NULL) AS ha_capability \
   FROM ai_price_catalog c \
   LEFT JOIN v_model_capabilities v \
          ON v.provider = c.provider AND v.model = c.model";

/// Fatti di catalogo per fornitore. Una sola query per l'intera risposta: gli
/// handler di stato elencano tutti i fornitori, e una query per fornitore
/// sarebbe N round-trip per la stessa tabella.
///
/// Legge le colonne su cui si decide, e nient'altro: un `SELECT *` legherebbe
/// questo modulo a colonne che non gli servono e che cambiano.
pub async fn carica_fatti_catalogo(db: &sqlx::PgPool) -> HashMap<String, Vec<ModelFact>> {
    let rows = sqlx::query(SQL_FATTI_CATALOGO)
        .fetch_all(db)
        .await
        .unwrap_or_default();
    let mut out: HashMap<String, Vec<ModelFact>> = HashMap::new();
    for r in rows {
        let provider: String = r.try_get("provider").unwrap_or_default();
        out.entry(provider).or_default().push(ModelFact {
            is_enabled: r.try_get("is_enabled").unwrap_or(false),
            capability_source: r
                .try_get("capability_source")
                .unwrap_or_else(|_| "auto".to_string()),
            auto_disabled_reason: r
                .try_get::<Option<String>, _>("auto_disabled_reason")
                .ok()
                .flatten(),
            ha_capability: r.try_get("ha_capability").unwrap_or(false),
        });
    }
    out
}

/// Copertura della dichiarazione di un fornitore. Chiuso: ogni variante ha un
/// rimedio diverso, ed e' per questo che sono varianti e non un conteggio nudo
/// (regola Q).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclarationCoverage {
    /// Nessun modello ABILITATO: non c'e' nulla da dichiarare, e una riga di
    /// capability per un modello che nessuno instradera' non servirebbe a nulla.
    NothingToDeclare,
    /// Ogni modello abilitato ha la sua riga.
    Complete { models: usize },
    /// Alcuni modelli abilitati sono scoperti. E' la forma che prende il
    /// catalogo VIVO: la migrazione del fornitore c'e', il discovery ha aggiunto
    /// modelli dopo di lei. Il rimedio e' per-modello e ricorrente.
    Partial { declared: usize, undeclared: usize },
    /// NESSUN modello abilitato ha una riga: il fornitore e' interamente fuori
    /// dalla vista. E' la forma che prende l'ONBOARDING senza la sua migrazione,
    /// e il rimedio e' un atto solo.
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
    /// runtime scrive capability: aspettare non cambia nulla. E' il campo su cui
    /// un allarme decide, mai una stringa.
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
/// produrrebbe un allarme che nessun intervento puo' spegnere —
/// `ai_price_catalog` porta 528 righe contro le 128 abilitate, e quasi tutte le
/// disabilitate sono modelli storici che nessuno dichiarera' mai.
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let c = classifica_dichiarazione(&vec![modello(true, false); 17]);
        assert_eq!(c, DeclarationCoverage::Absent { undeclared: 17 });
        assert!(c.richiede_intervento());
    }

    #[test]
    fn openai_e_parziale_non_assente() {
        // 65 abilitati, 11 scoperti: la migrazione c'e', il discovery ha
        // aggiunto modelli dopo. Rimedio diverso da quello di openrouter.
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
        let c = classifica_dichiarazione(&[modello(false, false)]);
        assert_eq!(c, DeclarationCoverage::NothingToDeclare);
        assert!(!c.richiede_intervento());
        assert_eq!(
            classifica_dichiarazione(&[]),
            DeclarationCoverage::NothingToDeclare
        );
    }

    #[test]
    fn la_copertura_completa_non_chiede_interventi() {
        let c = classifica_dichiarazione(&vec![modello(true, true); 4]);
        assert_eq!(c, DeclarationCoverage::Complete { models: 4 });
        assert!(!c.richiede_intervento());
        assert_eq!(c.undeclared(), 0);
    }

    #[test]
    fn i_fatti_si_chiedono_alla_vista() {
        // MUTAZIONE (regola O): sostituire `v_model_capabilities` con
        // `nexus_provider_capabilities` dentro SQL_FATTI_CATALOGO fa rosseggiare
        // QUESTO test, e nessun altro.
        //
        // Il test sqlx qui sotto NON puo' accorgersene, ed e' bene dirlo invece
        // di lasciarlo credere: la vista e' definita `FROM
        // nexus_provider_capabilities cap LEFT JOIN ...`, quindi oggi le due
        // fonti hanno lo STESSO insieme di coppie (provider, model) e per la
        // domanda «e' dichiarato?» rispondono identicamente. Il vincolo guarda
        // al futuro — il giorno in cui la vista filtrasse o unisse righe, la
        // copertura misurerebbe qualcosa di diverso da cio' che i consumatori
        // leggono — e un vincolo che vale solo in futuro si difende con
        // un'asserzione sul TESTO della query, non con un esperimento che oggi
        // non ha nulla da distinguere.
        assert!(
            SQL_FATTI_CATALOGO.contains("v_model_capabilities"),
            "i fatti devono arrivare dalla vista che i consumatori interrogano"
        );
        assert!(
            !SQL_FATTI_CATALOGO.contains("JOIN nexus_provider_capabilities"),
            "chiedere alla tabella e' una seconda idea di 'dichiarato'"
        );
    }

    /// La query risponde davvero, sullo schema che le migrazioni producono.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_copertura_si_misura_sulla_vista_che_la_produzione_legge(pool: sqlx::PgPool) {
        // Il trigger del gate 0629 respinge a `is_enabled=false` ogni riga senza
        // prova di probe: si abilitano dandogli quella prova, invece di seminare
        // lo stato finale a mano.
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

        sqlx::query(
            "INSERT INTO nexus_provider_capabilities (provider, model) \
             VALUES ('zeta', 'zeta-modello-0')",
        )
        .execute(&pool)
        .await
        .expect("seed capability");

        // PREMESSA misurata: la vista vede uno solo dei due.
        let nella_vista: i64 =
            sqlx::query_scalar("SELECT count(*) FROM v_model_capabilities WHERE provider = 'zeta'")
                .fetch_one(&pool)
                .await
                .expect("conteggio vista");
        assert_eq!(
            nella_vista, 1,
            "la vista parte da nexus_provider_capabilities: un modello senza riga \
             non vi compare affatto"
        );

        let fatti = carica_fatti_catalogo(&pool).await;
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
        let fatti = carica_fatti_catalogo(&pool).await;
        let modelli = fatti.get("zeta").cloned().unwrap_or_default();
        let c = classifica_dichiarazione(&modelli);
        assert_eq!(c, DeclarationCoverage::Complete { models: 2 });
        assert!(!c.richiede_intervento());
    }
}
