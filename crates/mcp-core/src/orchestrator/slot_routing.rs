//! Il canale SLOT della selezione modello: la DELEGA al servizio unico
//! (`select_model`) e il percorso storico che serve a flag OFF, fianco a
//! fianco per lo shadow-compare (fase 3, lotto 2).
//!
//! # Il difetto che chiude
//!
//! Il canale slot era l'ultimo percorso di selezione del turno PRIMARIO fuori
//! dal servizio unico: `select_models_for_requirement` caricava il catalog con
//! lo scoring a pesi e deduplicava top-1-per-provider, poi
//! `pick_slot_candidate_out_of_cooldown` applicava il cooldown DOPO la dedup e
//! per solo FORNITORE. Rispetto a `select_model` mancavano:
//!
//! - il PAVIMENTO di tier agentico (I8): uno slot `light` serviva un run
//!   agentico con un modello light anche col pavimento DB a 'medium';
//! - il cooldown per COPPIA prima della scelta: il modello saturo di un
//!   fornitore restava il candidato (era il top-1 dello scoring) e quello sano
//!   dello stesso fornitore non veniva mai proposto;
//! - l'esito tipizzato (I6): `None` muto invece di `NoModelReason`;
//! - il riordino cache-aware di CostFirst (lotto 1) e quello governato (ADR
//!   0030), che vivono dentro il servizio.
//!
//! (Il gate di qualificazione NON e' fra gli assi di divergenza: la fase 3b
//! del consolidamento lo aveva gia' portato in `load_catalog`. Deviazione
//! dichiarata dal design F3, che lo elencava fra i guadagni della delega: qui
//! la delega lo GARANTISCE per costruzione invece che per diligenza del
//! percorso, e il test lo fissa.)
//!
//! # Rollout a flag, non cutover secco (mig 0724)
//!
//! La delega cambia piu' assi insieme (pavimento, cooldown per coppia, esito):
//! senza una finestra di osservazione non si saprebbe quale asse spiega una
//! divergenza percepita in chat. Percio':
//!
//! - `routing.slots_via_select_model` = 'false' (stadio 1): serve il percorso
//!   storico, BIT-IDENTICO, e lo shadow-compare calcola la delega in parallelo
//!   loggando la divergenza (target `routing_shadow`, come il precedente
//!   `shadow_compare_per_intent`);
//! - flip a 'true' (stadio 2) dopo la finestra di osservazione; rollback =
//!   flip a 'false', zero deploy — ed e' la ragione per cui il percorso
//!   storico (e le colonne che legge: required_capabilities, cost_direction)
//!   NON si rimuove in questo giro. La pulizia e' lo stadio 3.
//! - `routing.slots_select_model_shadow` = 'true': lo shadow si puo' spegnere
//!   senza deploy se risultasse rumoroso (regola G). A flag ON e' inerte.

use sqlx::PgPool;

use super::model_routing;
use super::model_service::{
    self, ModelChoice, ModelRequest, NoModelReason, Profile, Rank, TierPolicy,
};
use crate::routing_slots::{ActionSlots, SlotRequirement};

/// Flag di rollout della delega (mig 0724). A OFF serve il percorso storico.
const SLOTS_VIA_SELECT_MODEL_KEY: &str = "routing.slots_via_select_model";

/// Interruttore dello shadow-compare a flag OFF (mig 0724).
const SLOTS_SHADOW_KEY: &str = "routing.slots_select_model_shadow";

/// `true` se la delega al servizio unico e' accesa (default OFF: nessuna
/// funzione si accende da sola, stessa disciplina di `cache_aware_enabled`).
async fn slots_via_select_model_enabled(db: &PgPool) -> bool {
    nexus_auth::get_bool_setting_or(db, SLOTS_VIA_SELECT_MODEL_KEY, false).await
}

/// `true` se lo shadow-compare va calcolato a flag OFF. Il default nel codice
/// e' `false` (chiave assente = migrazione non applicata = niente shadow); il
/// seed della mig 0724 lo accende, perche' lo stadio 1 ESISTE per osservare.
async fn shadow_enabled(db: &PgPool) -> bool {
    nexus_auth::get_bool_setting_or(db, SLOTS_SHADOW_KEY, false).await
}

/// La decisione del canale slot per un requisito gia' risolto dal lookup della
/// matrice. `None` = fallback al routing intent classico (come sempre).
///
/// Estratta da `Orchestrator::route_by_slots` perche' sia interrogabile dai
/// test per la STESSA strada della produzione (regola O): confidence e lookup
/// restano al chiamante, che conosce la matrice.
pub(super) async fn decidi(
    db: &PgPool,
    req: &SlotRequirement,
    slots: &ActionSlots,
) -> Option<(String, String)> {
    if slots_via_select_model_enabled(db).await {
        return match select_for_slot_requirement(db, req).await {
            Ok(scelta) => {
                tracing::info!(
                    "route_by_slots: slots=({}, {}, {}, {}) tier={} -> {}/{} via select_model ({})",
                    slots.action_verb,
                    slots.target_type,
                    slots.framework,
                    slots.scope,
                    req.preferred_tier,
                    scelta.provider,
                    scelta.model,
                    scelta.rationale,
                );
                Some((scelta.provider, scelta.model))
            }
            Err(reason) => {
                // Esaurimento TIPIZZATO (I6): la causa arriva dal servizio e si
                // riporta, non si reinterpreta. Il fallback resta il routing
                // intent classico, come per il percorso storico.
                tracing::warn!(
                    "route_by_slots: delega select_model senza modello ({reason}), \
                     fallback intent classico"
                );
                None
            }
        };
    }
    // Flag OFF: percorso storico, bit-identico allo storico.
    let servito = match slot_routing_candidates(db, req, slots).await {
        Some(candidati) => pick_slot_candidate_out_of_cooldown(&candidati, req, slots),
        None => None,
    };
    // Shadow best-effort: misura la delega senza servirla. Una divergenza qui
    // e' ATTESA e spiegabile (pavimento, cooldown per coppia, riordino):
    // e' esattamente cio' che la finestra di osservazione deve contare.
    if shadow_enabled(db).await {
        esito_shadow(db, req, servito.as_ref()).await.logga(slots);
    }
    servito
}

/// Traduce il requisito slot nel contratto del servizio unico e seleziona.
///
/// IL PAVIMENTO ALZA la richiesta quando il requisito e' agentico
/// (`requires_tool_use`): delega a [`model_routing::floor_tier_for_agentic`],
/// lo stesso punto che governa il selettore dinamico del turno primario — il
/// canale slot decide lo STESSO turno, quindi lo stesso pavimento. NON e' il
/// criterio di `purpose_floor` (che non alza mai la richiesta): quello vale
/// per i task interni, dove un purpose 'light' e' una scelta di costo
/// deliberata. Il design F3 citava purpose_floor come criterio, ma il suo
/// stesso test vincolante (lo slot `light` NON esce se un medium e' sano)
/// pretende l'innalzamento: deviazione dichiarata, risolta a favore del test.
///
/// `Flexible` e non `Degrade`: sotto il pavimento non si scende MAI (e' il
/// guadagno I8); se il pavimento e i tier sopra sono vuoti l'esito e' un
/// `NoModelReason` e il chiamante cade sul routing intent classico, che ha il
/// suo percorso graceful.
async fn select_for_slot_requirement(
    db: &PgPool,
    req: &SlotRequirement,
) -> Result<ModelChoice, NoModelReason> {
    let globale = model_routing::agentic_min_tier(db).await;
    let floor =
        model_routing::floor_tier_for_agentic(req.requires_tool_use, &req.preferred_tier, &globale);
    let request = ModelRequest {
        tier: &req.preferred_tier,
        tier_policy: TierPolicy::Flexible,
        profile: if req.requires_tool_use {
            Profile::Agentic
        } else {
            Profile::NonAgentic
        },
        capability: req.base_capability.as_deref(),
        min_context_window: 0,
        min_tier: Some(floor),
        exclude_providers: &[],
        pin: None,
        // Cache-aware dal lotto 1 (flag mig 0721) quando acceso.
        rank: Rank::CostFirst,
        // E' la selezione del turno primario: riordino telemetria-aware
        // (ADR 0030) come per il selettore dinamico del catalog.
        governed: true,
    };
    model_service::select_model(db, &request).await
}

/// L'esito dello shadow-compare, in CAMPI (regola Q): i test decidono su
/// questo, il log e' la resa.
struct SlotShadowEsito {
    /// Cio' che il percorso storico ha servito (None = fallback classico).
    pub servito: Option<(String, String)>,
    /// Cio' che la delega avrebbe servito, con l'esito tipizzato del servizio.
    pub delega: Result<ModelChoice, NoModelReason>,
    /// `true` quando i due percorsi coincidono (stessa coppia, o entrambi
    /// senza modello).
    pub is_match: bool,
}

impl SlotShadowEsito {
    /// La resa a log, target `routing_shadow` come il precedente per-intent.
    fn logga(&self, slots: &ActionSlots) {
        let (servito_provider, servito_model) = self
            .servito
            .as_ref()
            .map(|(p, m)| (p.as_str(), m.as_str()))
            .unwrap_or(("__none__", ""));
        match &self.delega {
            Ok(c) => tracing::info!(
                target: "routing_shadow",
                action_verb = %slots.action_verb,
                target_type = %slots.target_type,
                framework = %slots.framework,
                scope = %slots.scope,
                servito_provider,
                servito_model,
                delega_provider = %c.provider,
                delega_model = %c.model,
                delega_rationale = %c.rationale,
                is_match = self.is_match,
                "FASE3 shadow-compare slot (storico vs select_model)"
            ),
            Err(reason) => tracing::info!(
                target: "routing_shadow",
                action_verb = %slots.action_verb,
                target_type = %slots.target_type,
                framework = %slots.framework,
                scope = %slots.scope,
                servito_provider,
                servito_model,
                delega_reason = %reason,
                is_match = self.is_match,
                "FASE3 shadow-compare slot (storico vs select_model)"
            ),
        }
    }
}

/// Calcola lo shadow: la delega in parallelo alla decisione gia' servita.
/// Best-effort e senza effetti: e' una SELECT in piu', mai un cambio di rotta.
async fn esito_shadow(
    db: &PgPool,
    req: &SlotRequirement,
    servito: Option<&(String, String)>,
) -> SlotShadowEsito {
    let delega = select_for_slot_requirement(db, req).await;
    let is_match = match (servito, &delega) {
        (Some((p, m)), Ok(c)) => c.provider == *p && c.model == *m,
        // Entrambi senza modello: nessuna divergenza da contare.
        (None, Err(_)) => true,
        _ => false,
    };
    SlotShadowEsito {
        servito: servito.cloned(),
        delega,
        is_match,
    }
}

/// Candidati (provider, model) sani per una richiesta slot-routed, dal punto
/// unico tier-based `select_models_for_requirement`: ordinati per score, uno per
/// provider — la rotazione provider per disponibilita' vale quindi anche per le
/// richieste slot-routed. `None` (col medesimo logging) sia su errore di
/// selezione sia su lista vuota, cosi' il chiamante cade sul routing intent
/// classico. E' il percorso STORICO: serve a flag OFF e muore con lo stadio 3.
async fn slot_routing_candidates(
    db: &PgPool,
    req: &SlotRequirement,
    slots: &ActionSlots,
) -> Option<Vec<(String, String)>> {
    let candidates = match crate::routing_matrix_auto_promoter::select_models_for_requirement(
        db,
        &req.preferred_tier,
        &req.required_capabilities,
        req.requires_tool_use,
        &req.cost_direction,
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "route_by_slots: selezione tier-based fallita ({e}), fallback intent classico"
            );
            return None;
        }
    };
    if candidates.is_empty() {
        tracing::debug!(
            "route_by_slots: nessun candidato per tier {} (slots {}, {}, {}, {})",
            req.preferred_tier,
            slots.action_verb,
            slots.target_type,
            slots.framework,
            slots.scope,
        );
        return None;
    }
    Some(candidates)
}

/// Cooldown-awareness del percorso STORICO: ritorna il primo candidato con
/// provider NON in cooldown, oppure `None` (col WARN "tutti in cooldown") se
/// non ce n'e' nessuno. Guarda il solo FORNITORE, dopo la dedup: e' il difetto
/// censito che la delega chiude (il cooldown per coppia lo vede solo
/// `select_model`, via `esclusioni_selezione`). Resta bit-identico per il
/// flag OFF e per il rollback.
fn pick_slot_candidate_out_of_cooldown(
    candidates: &[(String, String)],
    req: &SlotRequirement,
    slots: &ActionSlots,
) -> Option<(String, String)> {
    let mut skipped: Vec<String> = Vec::new();
    for (provider, model) in candidates {
        if crate::provider_cooldown::is_provider_in_cooldown(provider) {
            skipped.push(provider.clone());
            continue;
        }
        if !skipped.is_empty() {
            tracing::info!(
                "route_by_slots: skip provider in cooldown [{}], scelto {}/{} (tier {}, pos {}/{})",
                skipped.join(","),
                provider,
                model,
                req.preferred_tier,
                skipped.len() + 1,
                candidates.len(),
            );
        } else {
            tracing::info!(
                "route_by_slots: slots=({}, {}, {}, {}) tier={} → {}/{}",
                slots.action_verb,
                slots.target_type,
                slots.framework,
                slots.scope,
                req.preferred_tier,
                provider,
                model,
            );
        }
        return Some((provider.clone(), model.clone()));
    }
    // Tutti i provider candidati sono in cooldown.
    tracing::warn!(
        "route_by_slots: TUTTI i {} provider candidati in cooldown [{}], fallback intent classico",
        candidates.len(),
        skipped.join(",")
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    /// Un requisito slot come lo produce il lookup della matrice; i campi del
    /// percorso storico (required_capabilities, cost_direction) restano
    /// compilati perche' a flag OFF li legge ancora lui.
    fn requisito(tier: &str, tool: bool) -> SlotRequirement {
        SlotRequirement {
            preferred_tier: tier.into(),
            required_capabilities: vec!["code".into()],
            requires_tool_use: tool,
            cost_direction: "asc".into(),
            base_capability: Some("code".into()),
        }
    }

    fn slots_di_prova() -> ActionSlots {
        ActionSlots {
            action_verb: "write".into(),
            target_type: "code".into(),
            framework: "*".into(),
            scope: "single".into(),
            confidence: 0.9,
        }
    }

    /// Semina un modello QUALIFICATO: sotto META_MIGRATOR il gate di routing
    /// e' ACCESO dal seed reale (mig 0595), quindi il seed porta l'evidenza
    /// che il gate pretende invece di aggirarlo.
    async fn semina_modello(pool: &PgPool, provider: &str, model: &str, tier: &str, costo: f64) {
        sqlx::query(
            "INSERT INTO ai_price_catalog \
               (provider, model, performance_tier, input_cost_per_million_tokens, \
                output_cost_per_million_tokens, currency, is_enabled, supports_tool_use, \
                agentic_thinking_policy, capabilities, qualified_capabilities, \
                context_window, pricing_state, qualification_state, \
                qualification_expires_at, last_probe_healthy_at) \
             VALUES ($1,$2,$3,$4,1.0,'USD',TRUE,TRUE,'none','[\"code\"]'::jsonb, \
                     '[\"code\"]'::jsonb,200000,'priced','qualified', \
                     now() + interval '30 days',now())",
        )
        .bind(provider)
        .bind(model)
        .bind(tier)
        .bind(costo)
        .execute(pool)
        .await
        .expect("seed catalog slot");
    }

    async fn pulisci_catalog(pool: &PgPool) {
        // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(pool)
            .await
            .expect("pulizia catalog");
    }

    /// I8 per il canale slot: uno slot `light` con tool_use NON esce su un
    /// modello light quando il pavimento agentico (default DB 'medium') ha un
    /// candidato sano. Il light qui e' il PIU' economico: vincerebbe CostFirst
    /// se il pavimento mancasse, quindi il test non puo' passare per fortuna.
    ///
    /// MUTAZIONE (eseguita, vedi commit): in `select_for_slot_requirement`
    /// sostituire `Flexible` + `min_tier: Some(floor)` con `Degrade` +
    /// `min_tier: None` fa vincere il light e questo assert rosseggia.
    /// E' il gemello del test sul pavimento di `route_model_from_catalog`.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn lo_slot_routing_rispetta_il_pavimento_agentico(pool: PgPool) {
        pulisci_catalog(&pool).await;
        semina_modello(&pool, "prov-basso", "modello-light", "light", 0.1).await;
        semina_modello(&pool, "prov-fascia", "modello-medium", "medium", 5.0).await;
        let scelta = select_for_slot_requirement(&pool, &requisito("light", true))
            .await
            .expect("un candidato nel pavimento c'e'");
        assert_eq!(
            (scelta.provider.as_str(), scelta.model.as_str()),
            ("prov-fascia", "modello-medium"),
            "lo slot light serve un run agentico: sotto il pavimento non si scende"
        );
    }

    /// Il cooldown per COPPIA precede la scelta: il modello saturo di un
    /// fornitore e' escluso PRIMA, e quello sano dello STESSO fornitore serve.
    /// Il percorso storico non poteva: la dedup top-1-per-provider avveniva
    /// prima del cooldown (per solo fornitore), quindi il saturo restava il
    /// candidato — mutazione DIRETTA del difetto censito, dimostrata qui
    /// chiamando il percorso storico sugli stessi dati.
    ///
    /// MUTAZIONE (eseguita, vedi commit): spegnere `apply_cooldown` in
    /// `filter_for` (model_service) fa vincere il saturo e il primo assert
    /// rosseggia.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_cooldown_per_coppia_precede_la_dedup(pool: PgPool) {
        // Nomi propri di questo test: lo stato del cooldown e' globale al processo.
        let fornitore = "__slot_coppia";
        pulisci_catalog(&pool).await;
        semina_modello(&pool, fornitore, "modello-saturo", "medium", 0.1).await;
        semina_modello(&pool, fornitore, "modello-sano", "medium", 2.0).await;

        // Il rate limit colpisce UN modello: e' un tetto suo, non del fornitore.
        crate::provider_cooldown::metti_in_cooldown_breve(
            fornitore,
            Some("modello-saturo"),
            "Rate limit raggiunto",
            60,
        );
        let scelta = select_for_slot_requirement(&pool, &requisito("medium", true))
            .await
            .expect("il fornitore ha un modello sano");
        assert_eq!(
            (scelta.provider.as_str(), scelta.model.as_str()),
            (fornitore, "modello-sano"),
            "la coppia in cooldown non si propone; l'altro modello dello stesso \
             fornitore ha quota propria e serve"
        );

        // Il percorso storico, sugli stessi dati, serve il SATURO: la dedup
        // top-1-per-provider lo tiene (e' il migliore per score) e il check di
        // cooldown guarda il solo fornitore, che non e' in cooldown.
        let vecchio = slot_routing_candidates(&pool, &requisito("medium", true), &slots_di_prova())
            .await
            .and_then(|c| {
                pick_slot_candidate_out_of_cooldown(
                    &c,
                    &requisito("medium", true),
                    &slots_di_prova(),
                )
            });
        assert_eq!(
            vecchio,
            Some((fornitore.to_string(), "modello-saturo".to_string())),
            "premessa del difetto censito: il percorso storico sceglie la coppia satura"
        );
        crate::provider_cooldown::remove_cooldown(fornitore);
    }

    /// Il gate di qualificazione vale anche per il canale slot: un modello
    /// senza evidenza (`qualification_state <> 'qualified'`) non esce dalla
    /// delega, e l'esito DICE che e' stato il gate (I6: GateEmpty, non un
    /// generico "nessun modello"). Il gate qui e' quello REALE letto dal seed
    /// della mig 0595 (enforce_routing_gate = true), non uno iniettato.
    ///
    /// MUTAZIONE dichiarata: se la delega passasse `Profile::NonAgentic` per un
    /// requisito con tool_use, il gate non si applicherebbe e il non
    /// qualificato uscirebbe (il test rosseggia sull'`expect_err`).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_gate_di_qualificazione_vale_anche_per_gli_slot(pool: PgPool) {
        pulisci_catalog(&pool).await;
        semina_modello(&pool, "prov-x", "modello-non-provato", "medium", 0.1).await;
        sqlx::query(
            "UPDATE ai_price_catalog SET qualification_state = 'unqualified' \
             WHERE model = 'modello-non-provato'",
        )
        .execute(&pool)
        .await
        .expect("squalifica");
        let esito = select_for_slot_requirement(&pool, &requisito("medium", true))
            .await
            .expect_err("il gate deve svuotare il pool");
        assert!(
            matches!(esito, NoModelReason::GateEmpty { .. }),
            "l'esito deve dire che e' stato il GATE, non un parco vuoto: {esito:?}"
        );
    }

    /// A flag OFF (seed mig 0724) la decisione SERVITA e' quella del percorso
    /// storico, bit-identica — anche dove la delega sceglierebbe altro — e lo
    /// shadow DICHIARA la divergenza in campi tipizzati (regola Q), senza
    /// cambiare la decisione.
    ///
    /// MUTAZIONE dichiarata: se `decidi` servisse la delega a flag OFF, il
    /// primo assert rosseggia (uscirebbe il medium del pavimento).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn flag_off_percorso_invariato_e_shadow_dichiara_la_divergenza(pool: PgPool) {
        pulisci_catalog(&pool).await;
        semina_modello(&pool, "prov-basso", "modello-light", "light", 0.1).await;
        semina_modello(&pool, "prov-fascia", "modello-medium", "medium", 5.0).await;
        let req = requisito("light", true);

        // Percorso servito: lo storico preferisce il tier richiesto (score
        // tier 1.0 sul light) e non conosce pavimento.
        let servito = decidi(&pool, &req, &slots_di_prova()).await;
        assert_eq!(
            servito,
            Some(("prov-basso".to_string(), "modello-light".to_string())),
            "a flag OFF serve il percorso storico, invariato"
        );

        // Lo shadow calcola la delega per la STESSA strada di `decidi`
        // (regola O) e la dichiara: divergenza attesa sul pavimento.
        let esito = esito_shadow(&pool, &req, servito.as_ref()).await;
        assert!(!esito.is_match, "la divergenza va dichiarata, non assorbita");
        let delega = esito.delega.expect("la delega risolve nel pavimento");
        assert_eq!(
            (delega.provider.as_str(), delega.model.as_str()),
            ("prov-fascia", "modello-medium"),
            "la delega applica il pavimento che lo storico non ha"
        );
    }
}
