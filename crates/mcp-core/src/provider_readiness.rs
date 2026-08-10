//! «Che cosa SAPPIAMO della salute di questo fornitore — e se non sappiamo
//! nulla, qualcuno lo scoprira'?»
//!
//! PUNTO UNICO (regola L) dello stato di prontezza di un fornitore, e rimedio
//! alla forma con cui quello stato viaggiava: `healthy: Option<bool>`, dove
//! `null` significava contemporaneamente «non configurato», «configurato e mai
//! interrogato», «il gateway non risponde» e «nessuno lo interroghera' mai».
//! Quattro situazioni con rimedi OPPOSTI — nessuno, aspettare, riavviare un
//! servizio, intervento admin — rese con lo stesso pallino grigio e la stessa
//! riga «Stato sconosciuto». L'ignoto e' una VARIANTE dichiarata, non un valore
//! comodo (regola Q).
//!
//! MISURATO il 09/08/2026 sul DB meta vivo, dopo l'onboarding di `kimi`
//! (mig 0690, applicata alle 15:16 UTC): chiave presente, `kimi_enabled=true`,
//! 4 modelli a catalogo, 0 abilitati, 0 righe in `nexus_routing_matrix`, 0 in
//! `nexus_provider_health` e 0 in `nexus_provider_health_history`. Il pannello
//! lo mostrava «Stato sconosciuto», identico a un gateway spento.
//!
//! La CIRCOLARITA' sospettata — «il probe interroga i soli fornitori che il
//! routing conosce» — NON esiste: `provider_health_probe::probed_providers`
//! legge `ai_price_catalog WHERE is_enabled = true`, mai la routing matrix. Il
//! motivo per cui kimi non era interrogato e' un altro, ed e' PREVISTO:
//! il trigger `ai_price_catalog_enforce_probe_before_enable` (mig 0629)
//! respinge a `is_enabled=false` ogni modello privo di `last_probe_healthy_at`
//! e lo marchia `unverified_no_probe`. Quel marchio e' l'ingresso al ciclo di
//! guarigione (`model_health_probe::run_reprobe_phase`), che infatti lo
//! RAGGIUNGE — nel log di mcp-core i candidati al re-probe passano da 13 a 17
//! nel primo round successivo alla migrazione, e i 4 in piu' sono i modelli
//! kimi. Il sistema stava lavorando: semplicemente non lo diceva a nessuno.
//!
//! Il criterio non e' indovinato: chiede ai DUE cicli reali se raggiungono un
//! modello, delegando ai loro predicati (regola L/O) invece di ricopiarli. Se
//! domani `is_reprobe_candidate` si restringe, un fornitore che nessuno
//! verifichera' piu' smette di dichiararsi «in attesa» e passa a `Stalled`
//! senza che nessuno debba ricordarsi di aggiornare questo modulo.

/// Il fatto di catalogo su cui i due cicli decidono vive nel crate
/// `nexus-capability-audit` insieme alla query che lo produce e all'altra
/// domanda che vi si appoggia (la copertura della dichiarazione). Sta li' e non
/// qui perche' anche uno strumento a riga di comando deve poterlo chiedere, e
/// mcp-core e' bin-only: l'alternativa non era «xtask chiama mcp-core», era
/// «xtask ricopia la query» (regola O).
pub use nexus_capability_audit::{carica_fatti_catalogo, ModelFact};

/// Quale ciclo di verifica raggiunge un modello. Sono due e sono nominati:
/// «qualcuno lo guardera'» senza dire CHI non permette di stimare QUANDO, ne'
/// di accorgersi che quel qualcuno e' stato spento.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CicloDiVerifica {
    /// `provider_health_probe`: interroga i fornitori con almeno un modello
    /// ABILITATO a catalogo (`probed_providers`).
    ProbePeriodico,
    /// `model_health_probe::run_reprobe_phase`: interroga i modelli disabilitati
    /// che `is_reprobe_candidate` ammette.
    ReProbe,
}

impl CicloDiVerifica {
    /// Identificatore canonico sul wire (regola N).
    pub fn wire(self) -> &'static str {
        match self {
            CicloDiVerifica::ProbePeriodico => "periodic_probe",
            CicloDiVerifica::ReProbe => "reprobe",
        }
    }
}

/// Perche' di un fornitore non si sa nulla, quando nessuno lo scoprira'. E' la
/// meta' che merita un intervento umano, e va distinta dall'attesa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CausaStallo {
    /// Nessun modello a catalogo: non c'e' nulla da interrogare, e nessun ciclo
    /// di verifica crea modelli. Solo il discovery o una migrazione lo fanno.
    NoModels,
    /// Modelli a catalogo, tutti disabilitati, e NESSUNO raggiunto da un ciclo.
    /// E' il limbo delle righe con `is_enabled=false` e `auto_disabled_reason`
    /// NULL: il probe periodico carica i soli abilitati, il re-probe pretende
    /// un reason, quindi non le vede nessuno dei due.
    NoVerificationCycle { models: usize },
}

impl CausaStallo {
    /// Identificatore canonico sul wire (regola N).
    pub fn wire(&self) -> &'static str {
        match self {
            CausaStallo::NoModels => "no_models",
            CausaStallo::NoVerificationCycle { .. } => "no_verification_cycle",
        }
    }
}

/// Stato di prontezza di un fornitore. Chiuso: ogni variante ha un rimedio
/// diverso, ed e' per questo che sono varianti e non un `Option<bool>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderReadiness {
    /// Chiave assente: non e' uno stallo, e' una scelta. Nessun rimedio.
    NotConfigured,
    /// Configurato, nessuna osservazione ANCORA, ma un ciclo di verifica lo
    /// raggiunge: transitorio, il rimedio e' aspettare il prossimo round.
    AwaitingFirstProbe {
        cycle: CicloDiVerifica,
        models: usize,
    },
    /// Configurato, nessuna osservazione, e nessuno la produrra': serve un
    /// intervento. E' il vicolo cieco che «Stato sconosciuto» nascondeva.
    Stalled(CausaStallo),
    /// C'e' una misura: e' l'unico caso in cui `healthy` significa qualcosa.
    Observed { healthy: bool },
}

impl ProviderReadiness {
    /// Identificatore canonico sul wire (regola N). `healthy`/`down` restano
    /// distinti da `observed{healthy}` perche' il consumatore che li legge non
    /// deve aprire un secondo campo per sapere il verdetto.
    pub fn wire(&self) -> &'static str {
        match self {
            ProviderReadiness::NotConfigured => "not_configured",
            ProviderReadiness::AwaitingFirstProbe { .. } => "awaiting_first_probe",
            ProviderReadiness::Stalled(_) => "stalled",
            ProviderReadiness::Observed { healthy: true } => "healthy",
            ProviderReadiness::Observed { healthy: false } => "down",
        }
    }

    /// `true` quando lo stato pretende un intervento umano: nessun automatismo
    /// lo sciogliera'. Il consumatore che accende un allarme legge QUESTO, non
    /// una stringa.
    pub fn richiede_intervento(&self) -> bool {
        matches!(self, ProviderReadiness::Stalled(_))
    }
}

/// Quale ciclo di verifica raggiunge QUESTO modello, se qualcuno lo raggiunge.
///
/// Non ricopia i due criteri: li INTERROGA. Il primo e' il predicato con cui
/// `provider_health_probe::probed_providers` filtra il catalogo
/// (`is_enabled = true`, verificato dal test che attraversa quella funzione
/// reale); il secondo e' `model_health_probe::is_reprobe_candidate` chiamato
/// per nome. Due copie divergerebbero in silenzio, ed e' la classe di difetto
/// che la regola O descrive: lo strumento e il sistema che rispondono a due
/// domande diverse credendo di rispondere alla stessa.
fn ciclo_che_raggiunge(m: &ModelFact) -> Option<CicloDiVerifica> {
    if m.is_enabled {
        return Some(CicloDiVerifica::ProbePeriodico);
    }
    if crate::model_health_probe::is_reprobe_candidate(
        m.is_enabled,
        &m.capability_source,
        m.auto_disabled_reason.as_deref(),
    ) {
        return Some(CicloDiVerifica::ReProbe);
    }
    None
}

/// Classifica la prontezza di un fornitore dai fatti. PURA: nessun I/O, quindi
/// provabile senza DB — ma i fatti li raccoglie [`carica_fatti_catalogo`] dalla
/// stessa tabella che i due cicli leggono.
///
/// `configured` = chiave API presente (`fetch_api_key_configured`, la stessa
/// fonte del campo `configured` gia' sul wire: due criteri darebbero due idee
/// diverse di «configurato» nella stessa risposta HTTP).
/// `observed` = `healthy` dell'ultima riga di `nexus_provider_health_history`.
///
/// PRECEDENZA. La chiave viene prima di tutto: tolta la chiave, un'osservazione
/// storica descrive un fornitore che non e' piu' in servizio. L'osservazione
/// viene prima dell'attesa: dove c'e' una misura non si specula su chi
/// guardera'. Fra i due cicli vince il periodico, che gira ogni 5 minuti contro
/// i 30 del re-probe: quando entrambi si applicano, la prima risposta arrivera'
/// da quello.
pub fn classifica(
    configured: bool,
    models: &[ModelFact],
    observed: Option<bool>,
) -> ProviderReadiness {
    if !configured {
        return ProviderReadiness::NotConfigured;
    }
    if let Some(healthy) = observed {
        return ProviderReadiness::Observed { healthy };
    }
    if models.is_empty() {
        return ProviderReadiness::Stalled(CausaStallo::NoModels);
    }
    let mut periodico = 0usize;
    let mut reprobe = 0usize;
    for m in models {
        match ciclo_che_raggiunge(m) {
            Some(CicloDiVerifica::ProbePeriodico) => periodico += 1,
            Some(CicloDiVerifica::ReProbe) => reprobe += 1,
            None => {}
        }
    }
    if periodico > 0 {
        return ProviderReadiness::AwaitingFirstProbe {
            cycle: CicloDiVerifica::ProbePeriodico,
            models: periodico,
        };
    }
    if reprobe > 0 {
        return ProviderReadiness::AwaitingFirstProbe {
            cycle: CicloDiVerifica::ReProbe,
            models: reprobe,
        };
    }
    ProviderReadiness::Stalled(CausaStallo::NoVerificationCycle {
        models: models.len(),
    })
}

/// Scrive la prontezza sull'entry JSON di un fornitore. Unico compositore
/// (regola L): i due handler di stato — quello che parla col gateway e quello
/// interno — devono dire la STESSA cosa dello stesso fornitore, e prima
/// costruivano l'entry ognuno per conto proprio.
///
/// Il testo NON si compone qui (regola Q, punto 3): il wire porta i campi e la
/// UI ne fa una frase nella lingua dell'utente.
pub fn scrivi_prontezza(p: &mut serde_json::Value, readiness: &ProviderReadiness) {
    p["readiness"] = serde_json::json!(readiness.wire());
    match readiness {
        ProviderReadiness::AwaitingFirstProbe { cycle, models } => {
            p["readiness_cycle"] = serde_json::json!(cycle.wire());
            p["readiness_models"] = serde_json::json!(models);
        }
        ProviderReadiness::Stalled(causa) => {
            p["readiness_cause"] = serde_json::json!(causa.wire());
            if let CausaStallo::NoVerificationCycle { models } = causa {
                p["readiness_models"] = serde_json::json!(models);
            }
        }
        ProviderReadiness::NotConfigured | ProviderReadiness::Observed { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abilitato() -> ModelFact {
        ModelFact {
            is_enabled: true,
            capability_source: "auto".to_string(),
            auto_disabled_reason: None,
            ha_capability: true,
        }
    }

    /// Lo stato REALE di kimi il 09/08/2026 alle 15:16 UTC: disabilitato dal
    /// trigger del gate 0629 col marchio che apre il re-probe.
    fn in_attesa_di_verifica() -> ModelFact {
        ModelFact {
            is_enabled: false,
            capability_source: "auto".to_string(),
            auto_disabled_reason: Some(
                crate::model_health_probe::REASON_UNVERIFIED_NO_PROBE.to_string(),
            ),
            ha_capability: false,
        }
    }

    /// Il limbo: disabilitato SENZA reason. Il probe periodico carica i soli
    /// abilitati, il re-probe pretende un reason: non lo vede nessuno.
    fn nel_limbo() -> ModelFact {
        ModelFact {
            is_enabled: false,
            capability_source: "auto".to_string(),
            auto_disabled_reason: None,
            ha_capability: false,
        }
    }

    #[test]
    fn senza_chiave_non_e_uno_stallo() {
        // vllm sul DB vivo: nessuna chiave, nessun modello. Non deve accendere
        // nessun allarme — nessuno ha chiesto che funzionasse.
        assert_eq!(classifica(false, &[], None), ProviderReadiness::NotConfigured);
        assert!(!classifica(false, &[], None).richiede_intervento());
    }

    #[test]
    fn kimi_e_in_attesa_non_sconosciuto() {
        // I 4 modelli kimi, chiave presente, zero osservazioni.
        let modelli = vec![in_attesa_di_verifica(); 4];
        let r = classifica(true, &modelli, None);
        assert_eq!(
            r,
            ProviderReadiness::AwaitingFirstProbe {
                cycle: CicloDiVerifica::ReProbe,
                models: 4,
            }
        );
        assert_eq!(r.wire(), "awaiting_first_probe");
        // NON e' un intervento: il ciclo di guarigione lo raggiunge da solo.
        assert!(!r.richiede_intervento());
    }

    #[test]
    fn il_limbo_e_uno_stallo_che_pretende_un_intervento() {
        let modelli = vec![nel_limbo(), nel_limbo()];
        let r = classifica(true, &modelli, None);
        assert_eq!(
            r,
            ProviderReadiness::Stalled(CausaStallo::NoVerificationCycle { models: 2 })
        );
        assert!(r.richiede_intervento());
    }

    #[test]
    fn configurato_senza_modelli_e_uno_stallo() {
        // Chiave inserita dall'admin e nessuna migrazione di catalogo: nessun
        // ciclo crea modelli, quindi nessuno arrivera' mai a un'osservazione.
        let r = classifica(true, &[], None);
        assert_eq!(r, ProviderReadiness::Stalled(CausaStallo::NoModels));
        assert!(r.richiede_intervento());
    }

    #[test]
    fn un_modello_abilitato_e_atteso_dal_probe_periodico() {
        // Fornitore appena abilitato, probe non ancora girato: e' attesa, non
        // stallo, e il ciclo che lo raggiungera' e' l'altro.
        let r = classifica(true, &[abilitato()], None);
        assert_eq!(
            r,
            ProviderReadiness::AwaitingFirstProbe {
                cycle: CicloDiVerifica::ProbePeriodico,
                models: 1,
            }
        );
    }

    #[test]
    fn losservazione_precede_lattesa() {
        // Con una misura non si specula su chi guardera'.
        assert_eq!(
            classifica(true, &[in_attesa_di_verifica()], Some(true)),
            ProviderReadiness::Observed { healthy: true }
        );
        assert_eq!(
            classifica(true, &[abilitato()], Some(false)).wire(),
            "down"
        );
    }

    #[test]
    fn la_chiave_revocata_precede_losservazione_storica() {
        // Tolta la chiave, un healthy di ieri descrive un fornitore che non e'
        // piu' in servizio.
        assert_eq!(
            classifica(false, &[abilitato()], Some(true)),
            ProviderReadiness::NotConfigured
        );
    }

    #[test]
    fn il_criterio_del_reprobe_non_e_ricopiato() {
        // MUTAZIONE (regola O): `is_reprobe_candidate` esclude le righe con
        // reason `manual:%`. Se questo modulo avesse una copia del criterio,
        // continuerebbe a dichiarare "in attesa" un modello che il re-probe non
        // guarda piu'. Il test lo prova dal lato dei dati, senza toccare il
        // predicato: cambiando SOLO il reason lo stato deve ribaltarsi.
        let escluso = ModelFact {
            is_enabled: false,
            capability_source: "auto".to_string(),
            auto_disabled_reason: Some("manual:disabilitato dall'admin".to_string()),
            ha_capability: false,
        };
        assert_eq!(
            classifica(true, &[escluso], None),
            ProviderReadiness::Stalled(CausaStallo::NoVerificationCycle { models: 1 })
        );
        // Stesso modello, reason che il re-probe ammette: torna in attesa.
        assert!(matches!(
            classifica(true, &[in_attesa_di_verifica()], None),
            ProviderReadiness::AwaitingFirstProbe { .. }
        ));
    }

    /// Semina un fornitore nello stato ESATTO in cui la mig 0690 + il trigger
    /// del gate 0629 hanno lasciato kimi: chiave presente, modelli a catalogo
    /// tutti respinti a `is_enabled=false` col marchio `unverified_no_probe`,
    /// nessuna riga di routing, nessuna osservazione.
    ///
    /// Non semina cio' che il trigger produce: FA produrre al trigger. Le righe
    /// entrano con `is_enabled=true` esattamente come le scrive la migrazione, e
    /// se ne rileggono i valori — cosi' il test misura il gate reale invece di
    /// fissare l'assunto su cosa il gate faccia (regola O).
    async fn semina_fornitore_appena_onboardato(
        db: &sqlx::PgPool,
        provider: &str,
        modelli: usize,
    ) {
        sqlx::query(
            "INSERT INTO settings (key, value, category, description, is_secret) \
             VALUES ($1, 'chiave-di-test', 'providers', 'seed test', true) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
        )
        .bind(format!("{provider}_api_key"))
        .execute(db)
        .await
        .expect("seed api key");
        for i in 0..modelli {
            sqlx::query(
                "INSERT INTO ai_price_catalog \
                    (provider, model, display_name, input_cost_per_million_tokens, \
                     output_cost_per_million_tokens, currency, is_enabled, \
                     capability_source) \
                 VALUES ($1, $2, $2, 1.0, 1.0, 'USD', true, 'auto')",
            )
            .bind(provider)
            .bind(format!("{provider}-modello-{i}"))
            .execute(db)
            .await
            .expect("seed catalog");
        }
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn un_fornitore_appena_onboardato_e_in_attesa_e_il_reprobe_lo_raggiunge(
        pool: sqlx::PgPool,
    ) {
        semina_fornitore_appena_onboardato(&pool, "zeta", 4).await;

        // PREMESSA misurata, non assunta: il gate 0629 ha respinto le righe.
        let (abilitati, marchiati): (i64, i64) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE is_enabled), \
                    count(*) FILTER (WHERE auto_disabled_reason = 'unverified_no_probe') \
               FROM ai_price_catalog WHERE provider = 'zeta'",
        )
        .fetch_one(&pool)
        .await
        .expect("conteggio catalog");
        assert_eq!(
            (abilitati, marchiati),
            (0, 4),
            "il trigger del gate 0629 deve respingere a false e marchiare: senza \
             questa premessa il test misura un altro stato"
        );

        // IL PRODUTTORE 1 (regola O): la funzione REALE con cui il probe
        // periodico sceglie chi interrogare. Non lo raggiunge — ed e' corretto,
        // non ha modelli abilitati.
        let sondati = crate::provider_health_probe::probed_providers(&pool).await;
        assert!(
            !sondati.contains(&"zeta".to_string()),
            "probed_providers non deve vedere un fornitore senza modelli abilitati"
        );

        // IL PRODUTTORE 2 (regola O): la funzione REALE con cui il re-probe
        // sceglie i candidati. Questo lo raggiunge, ed e' la ragione per cui lo
        // stato e' ATTESA e non stallo.
        let candidati = crate::model_health_probe::load_reprobe_candidates(&pool)
            .await
            .expect("candidati re-probe");
        let zeta: Vec<&str> = candidati
            .iter()
            .filter(|c| c.provider == "zeta")
            .map(|c| c.model.as_str())
            .collect();
        assert_eq!(zeta.len(), 4, "il re-probe deve raggiungere i 4 modelli");

        // Il verdetto, dai fatti letti dalla STESSA tabella dai due cicli.
        let fatti = carica_fatti_catalogo(&pool).await;
        let modelli = fatti.get("zeta").cloned().unwrap_or_default();
        assert_eq!(
            classifica(true, &modelli, None),
            ProviderReadiness::AwaitingFirstProbe {
                cycle: CicloDiVerifica::ReProbe,
                models: 4,
            },
            "chiave presente + catalogo + zero osservazioni = attesa dichiarata, \
             mai un healthy=null indistinguibile da 'non configurato'"
        );

        // MUTAZIONE (regola O): si toglie ai modelli il marchio che li rende
        // candidati — e' il limbo delle 47 righe citate dalla mig 0690. Nessun
        // ciclo li raggiunge piu', e il verdetto DEVE ribaltarsi in stallo.
        sqlx::query(
            "UPDATE ai_price_catalog SET auto_disabled_reason = NULL WHERE provider = 'zeta'",
        )
        .execute(&pool)
        .await
        .expect("mutazione");
        assert!(
            crate::model_health_probe::load_reprobe_candidates(&pool)
                .await
                .expect("candidati")
                .iter()
                .all(|c| c.provider != "zeta"),
            "senza reason il re-probe non li vede piu'"
        );
        let fatti = carica_fatti_catalogo(&pool).await;
        let modelli = fatti.get("zeta").cloned().unwrap_or_default();
        let r = classifica(true, &modelli, None);
        assert_eq!(
            r,
            ProviderReadiness::Stalled(CausaStallo::NoVerificationCycle { models: 4 }),
            "nessun ciclo lo raggiunge: e' uno stallo, e va detto"
        );
        assert!(r.richiede_intervento());
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_criterio_del_probe_periodico_e_quello_che_il_probe_usa(pool: sqlx::PgPool) {
        // Il secondo dei due cicli non e' una funzione pura: e' un filtro SQL
        // dentro `probed_providers`. Il modo di non ricopiarlo e' interrogarlo
        // (regola O): stesso DB, stessi dati, si confronta cio' che il probe
        // sceglie con cio' che il classificatore dichiara.
        semina_fornitore_appena_onboardato(&pool, "zeta", 2).await;
        sqlx::query(
            "UPDATE ai_price_catalog \
                SET is_enabled = true, last_probe_healthy_at = NOW(), \
                    auto_disabled_reason = NULL, auto_disabled_at = NULL \
              WHERE provider = 'zeta' AND model = 'zeta-modello-0'",
        )
        .execute(&pool)
        .await
        .expect("abilita un modello con la prova che il gate pretende");

        let sondati = crate::provider_health_probe::probed_providers(&pool).await;
        assert!(
            sondati.contains(&"zeta".to_string()),
            "con un modello abilitato il probe periodico lo interroga"
        );

        let fatti = carica_fatti_catalogo(&pool).await;
        let modelli = fatti.get("zeta").cloned().unwrap_or_default();
        assert_eq!(
            classifica(true, &modelli, None),
            ProviderReadiness::AwaitingFirstProbe {
                cycle: CicloDiVerifica::ProbePeriodico,
                models: 1,
            },
            "il ciclo dichiarato deve essere quello che lo raggiunge davvero"
        );
    }

    #[test]
    fn il_wire_non_perde_il_dettaglio() {
        let mut p = serde_json::json!({ "name": "kimi" });
        scrivi_prontezza(
            &mut p,
            &classifica(true, &vec![in_attesa_di_verifica(); 4], None),
        );
        assert_eq!(p["readiness"], "awaiting_first_probe");
        assert_eq!(p["readiness_cycle"], "reprobe");
        assert_eq!(p["readiness_models"], 4);

        let mut q = serde_json::json!({ "name": "zeta" });
        scrivi_prontezza(&mut q, &classifica(true, &[], None));
        assert_eq!(q["readiness"], "stalled");
        assert_eq!(q["readiness_cause"], "no_models");
        // NoModels non ha un conteggio: non se ne inventa uno a zero.
        assert!(q.get("readiness_models").is_none());
    }
}
