//! Il test PARAMETRICO del servizio unico.
//!
//! Il punto non e' coprire qualche caso: e' che il costo di aggiungere un filtro
//! di eleggibilita' diventi "una riga nel servizio, ZERO nei test" — la matrice
//! lo verifica gia' su tutte le modalita'. E' l'opposto di com'e' andata finora:
//! il gate di qualificazione e' stato aggiunto a mano call site per call site, e
//! due siti (`route_by_slots`, `select_models_for_requirement`) sono rimasti
//! indietro senza che nulla arrossisse.

use super::*;

/// Il gate INIETTATO: i test non passano dalla cache statica di
/// `qualification_gate` (60s, in-process, condivisa fra i test paralleli), che
/// li renderebbe dipendenti dall'ordine di esecuzione (regola F).
fn gate(acceso: bool) -> crate::orchestrator::QualificationGate {
    crate::orchestrator::QualificationGate {
        require_qualified: acceso,
        exclude_preview: acceso,
    }
}

/// Un modello che viola UN filtro obbligatorio. Nessuna modalita' del servizio
/// puo' sceglierlo. Aggiungere un filtro domani = aggiungere UNA riga qui.
struct Veleno {
    model: &'static str,
    /// Le colonne che lo rendono NON eleggibile.
    colonne: &'static str,
    valori: &'static str,
    /// `true` se e' veleno solo per il profilo agentico (il non-agentico non
    /// filtra tool_use ne' thinking-policy: e' una scelta dichiarata).
    solo_agentico: bool,
    /// `true` se e' veleno solo col gate di qualificazione ACCESO.
    solo_col_gate: bool,
}

const VELENI: &[Veleno] = &[
    Veleno {
        model: "veleno-disabilitato",
        colonne: "is_enabled",
        valori: "false",
        solo_agentico: false,
        solo_col_gate: false,
    },
    Veleno {
        model: "veleno-senza-tool",
        colonne: "supports_tool_use",
        valori: "false",
        solo_agentico: true,
        solo_col_gate: false,
    },
    Veleno {
        model: "veleno-thinking-exclude",
        colonne: "agentic_thinking_policy",
        valori: "'exclude'",
        solo_agentico: true,
        solo_col_gate: false,
    },
    Veleno {
        model: "veleno-non-qualificato",
        colonne: "qualification_state",
        valori: "'unqualified'",
        solo_agentico: true,
        solo_col_gate: true,
    },
    Veleno {
        model: "veleno-preview",
        colonne: "qualification_state",
        valori: "'qualified'",
        solo_agentico: true,
        solo_col_gate: true,
    },
];

/// Semina: i veleni + UN SOLO modello sano, tutti nello stesso tier e con la
/// stessa capability. Se il servizio sceglie qualcosa di diverso dal sano, ha
/// scavalcato un filtro.
/// Semina il sano + SOLO i veleni pertinenti a questa modalita'. Cosi'
/// l'asserzione resta la piu' forte possibile ("l'unico esito e' il sano")
/// invece di ammettere eccezioni: un veleno `solo_agentico` NON e' veleno per il
/// profilo non-agentico (che per scelta dichiarata non filtra tool_use ne'
/// thinking-policy), e seminarlo li' proverebbe solo che il test e' confuso.
async fn seed(pool: &PgPool, tier: &str, profile: Profile, gate_acceso: bool) {
    // Schema REALE (regola O): il DELETE isola dal catalog di produzione e
    // pulisce il giro precedente quando `seed` e' richiamata in loop (il vecchio
    // DROP TABLE + create_ai_price_catalog_table serviva lo stesso scopo).
    sqlx::query("DELETE FROM ai_price_catalog")
        .execute(pool)
        .await
        .expect("pulizia catalog");
    // Il modello SANO: l'unica scelta ammissibile.
    sqlx::query(&format!(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
          performance_tier, capabilities, qualification_state, qualification_expires_at, \
          input_cost_per_million_tokens, output_cost_per_million_tokens, context_window, currency, \
          last_probe_healthy_at) VALUES \
         ('sano-provider', 'modello-sano', true, true, 'none', '{tier}', \
          '[\"reasoning\"]'::jsonb, 'qualified', now() + interval '30 days', 1.0, 1.0, 200000, 'USD', now())"
    ))
    .execute(pool)
    .await
    .expect("modello sano");

    // I veleni: nascono IDENTICI al sano, poi un UPDATE applica il veleno. Sono
    // i piu' ECONOMICI e con la finestra piu' AMPIA, cosi' vincerebbero
    // l'ordinamento in ogni Rank se il filtro mancasse: il test non puo' passare
    // per fortuna.
    for vel in VELENI {
        if vel.solo_agentico && profile != Profile::Agentic {
            continue;
        }
        if vel.solo_col_gate && !gate_acceso {
            continue;
        }
        sqlx::query(&format!(
            "INSERT INTO ai_price_catalog \
             (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
              performance_tier, capabilities, qualification_state, qualification_expires_at, \
              input_cost_per_million_tokens, output_cost_per_million_tokens, context_window, currency, \
              last_probe_healthy_at) VALUES \
             ('veleno-provider', '{}', true, true, 'none', '{tier}', \
              '[\"reasoning\"]'::jsonb, 'qualified', now() + interval '30 days', 0.01, 0.01, 999999, 'USD', now())",
            vel.model
        ))
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("veleno {}: {e}", vel.model));
        sqlx::query(&format!(
            "UPDATE ai_price_catalog SET {} = {} WHERE model = '{}'",
            vel.colonne, vel.valori, vel.model
        ))
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("veleno {} (update): {e}", vel.model));
    }
}

fn modalita() -> Vec<(TierPolicy, Profile, Rank)> {
    let mut v = Vec::new();
    for policy in [
        TierPolicy::Degrade,
        TierPolicy::Exact {
            why: ExactReason::ScaleTarget,
        },
        TierPolicy::AnyTier,
    ] {
        for profile in [Profile::Agentic, Profile::NonAgentic] {
            for rank in [
                Rank::CostFirst,
                Rank::FailoverSafe,
                Rank::NonAgenticSafe,
                Rank::WidestWindow,
            ] {
                v.push((policy, profile, rank));
            }
        }
    }
    v
}

/// I1/I2: NESSUNA modalita' sceglie un modello non eleggibile. E' il test che
/// rende il servizio un punto unico invece di una convenzione.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn nessuna_modalita_sceglie_un_modello_non_eleggibile(pool: PgPool) {
    for gate_acceso in [true, false] {
        for (policy, profile, rank) in modalita() {
            seed(&pool, "heavy", profile, gate_acceso).await;
            let req = ModelRequest {
                tier: "heavy",
                tier_policy: policy,
                profile,
                capability: Some("reasoning"),
                min_context_window: 0,
                min_tier: None,
                exclude_providers: &[],
                pin: None,
                rank,
                governed: false,
            };
            let out = select_model_with_gate(&pool, &req, gate(gate_acceso)).await;
            match out {
                Ok(choice) => assert_eq!(
                    choice.model, "modello-sano",
                    "modalita' (policy={policy:?}, profile={profile:?}, rank={rank:?}, \
                     gate={gate_acceso}) ha scelto un VELENO: {} — un filtro \
                     obbligatorio e' stato scavalcato",
                    choice.model
                ),
                // Un esito vuoto e' ammesso solo se TIPIZZATO (I6), mai un None muto.
                Err(reason) => assert!(
                    !matches!(reason, NoModelReason::InvalidRequest(_)),
                    "modalita' valida rifiutata come invalida: {reason:?}"
                ),
            }
        }
    }
}

/// I3: la degradazione e' MONOTONA — mai un tier PIU' capace del richiesto.
/// E con `Exact` il tier effettivo E' quello richiesto, sempre.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn degradazione_monotona_in_ogni_modalita(pool: PgPool) {
    // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
    sqlx::query("DELETE FROM ai_price_catalog")
        .execute(&pool)
        .await
        .expect("pulizia catalog");
    // Solo un 'medium' sano: chi chiede 'heavy' puo' solo degradare (o fallire).
    sqlx::query(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
          performance_tier, capabilities, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, \
          last_probe_healthy_at) VALUES \
         ('p', 'solo-medium', true, true, 'none', 'medium', '[\"reasoning\"]'::jsonb, 1.0, 1.0, 'USD', now())",
    )
    .execute(&pool)
    .await
    .expect("seed");

    // Degrade: scende a medium e lo DICHIARA.
    let req = ModelRequest::agentic("heavy").capability(Some("reasoning"));
    let c = select_model_with_gate(&pool, &req, gate(false)).await.expect("deve degradare");
    assert_eq!(c.model, "solo-medium");
    assert_eq!(c.effective_tier.as_deref(), Some("medium"));
    assert!(c.degraded, "I4: degraded deve essere un dato TRUE");
    assert_eq!(c.rationale, "tier=heavy:degraded_to=medium");
    assert!(
        tier_rank(c.effective_tier.as_deref().unwrap()) <= tier_rank("heavy"),
        "I3: monotonia violata"
    );

    // Exact: nessuna degradazione, esito vuoto TIPIZZATO e ATTESO.
    let req = ModelRequest::agentic("heavy")
        .capability(Some("reasoning"))
        .tier_policy(TierPolicy::Exact {
            why: ExactReason::ScaleTarget,
        });
    let err = select_model_with_gate(&pool, &req, gate(false)).await.expect_err("Exact non degrada");
    assert!(
        matches!(err, NoModelReason::ExactTierEmpty { why: ExactReason::ScaleTarget, .. }),
        "I6: l'esito deve dire PERCHE', e ScaleTarget e' un esito atteso: {err:?}"
    );
    assert!(err.is_expected(), "un upscale senza bersaglio non e' un guasto");
}

/// I5: il pin cede il PROVIDER, mai la qualita'. E' l'invariante che una
/// regressione ha gia' violato una volta oggi.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn il_pin_non_degrada_mai(pool: PgPool) {
    // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
    sqlx::query("DELETE FROM ai_price_catalog")
        .execute(&pool)
        .await
        .expect("pulizia catalog");
    sqlx::query(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
          performance_tier, capabilities, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, \
          last_probe_healthy_at) VALUES \
         ('altro',   'altro-medium',  true, true, 'none', 'medium', '[\"code\"]'::jsonb, 1.0, 1.0, 'USD', now()), \
         ('pinnato', 'pinnato-light', true, true, 'none', 'light',  '[\"code\"]'::jsonb, 0.5, 0.5, 'USD', now())",
    )
    .execute(&pool)
    .await
    .expect("seed");

    // Il provider pinnato NON ha un 'medium': esito vuoto, cosi' il chiamante
    // ritenta senza pin e prende il tier GIUSTO altrove. Mai degradare al light
    // del pin.
    let req = ModelRequest::agentic("medium")
        .capability(Some("code"))
        .pinned("pinnato");
    let err = select_model_with_gate(&pool, &req, gate(false)).await.expect_err(
        "col pin il tier non si degrada: deve tornare vuoto, non 'pinnato-light'",
    );
    assert!(
        matches!(err, NoModelReason::ExactTierEmpty { why: ExactReason::PinnedProvider, .. }),
        "{err:?}"
    );

    // Il costruttore `pinned` forza Exact: costruire "pin + Degrade" a mano e'
    // un errore del chiamante, non uno stato del parco.
    let req = ModelRequest {
        pin: Some("pinnato"),
        tier_policy: TierPolicy::Degrade,
        ..ModelRequest::agentic("medium")
    };
    let err = select_model_with_gate(&pool, &req, gate(false)).await.expect_err("I5 deve rifiutare");
    assert!(matches!(err, NoModelReason::InvalidRequest(_)), "{err:?}");
}

/// I6: il pool svuotato DAL GATE non si confonde col parco fermo. Oggi questa
/// differenza vive solo in un `tracing::warn!` e il chiamante non la vede — ed e'
/// la differenza fra "aspetta il worker" e "il parco e' giu'".
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn il_gate_che_svuota_il_pool_si_distingue_dal_parco_fermo(pool: PgPool) {
    // Schema REALE (regola O): il DELETE isola dal catalog di produzione. Il
    // gate qui e' INIETTATO (`gate(true)` sotto, non letto da `settings`): la
    // vecchia riga in `settings` era ridondante, `select_model_with_gate` non
    // la consulta.
    sqlx::query("DELETE FROM ai_price_catalog")
        .execute(&pool)
        .await
        .expect("pulizia catalog");
    // Un modello SANO ma NON qualificato: col gate acceso il pool e' vuoto, ma il
    // parco NON e' fermo. Il motivo deve dirlo.
    sqlx::query(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
          performance_tier, capabilities, qualification_state, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, \
          last_probe_healthy_at) VALUES \
         ('p', 'sano-ma-non-qualificato', true, true, 'none', 'medium', \
          '[\"code\"]'::jsonb, 'unqualified', 1.0, 1.0, 'USD', now())",
    )
    .execute(&pool)
    .await
    .expect("seed");

    let req = ModelRequest::agentic("medium").capability(Some("code"));
    let err = select_model_with_gate(&pool, &req, gate(true)).await.expect_err("il gate svuota il pool");
    assert!(
        matches!(err, NoModelReason::GateEmpty { .. }),
        "il motivo deve distinguere 'il gate ha svuotato il pool' da 'il parco e' \
         fermo': un worker di qualificazione rotto e un parco giu' richiedono \
         azioni OPPOSTE. Ricevuto: {err:?}"
    );
}

// Il test I7 su `Rank::HighestTierFirst` e' stato rimosso con la variante
// (fase 3, lotto 2): quel Rank non aveva call site di produzione. La scala
// tier->SQL resta presidiata da `tier_rank_sql_coincide_col_rank_rust` in
// model_selection (il ponte Rust<->Postgres del vocabolario) e dal min_tier.

/// I4: `degraded` e' coerente col tier effettivo, e NON scatta quando il tier
/// richiesto e' disponibile (niente ripieghi gratuiti).
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn degraded_non_scatta_se_il_tier_richiesto_c_e(pool: PgPool) {
    // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
    sqlx::query("DELETE FROM ai_price_catalog")
        .execute(&pool)
        .await
        .expect("pulizia catalog");
    sqlx::query(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
          performance_tier, capabilities, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, \
          last_probe_healthy_at) VALUES \
         ('p', 'heavy-caro',      true, true, 'none', 'heavy',  '[\"code\"]'::jsonb, 90.0, 90.0, 'USD', now()), \
         ('p', 'medium-economico', true, true, 'none', 'medium', '[\"code\"]'::jsonb, 0.1, 0.1, 'USD', now())",
    )
    .execute(&pool)
    .await
    .expect("seed");

    let req = ModelRequest::agentic("heavy").capability(Some("code"));
    let c = select_model_with_gate(&pool, &req, gate(false)).await.expect("il tier heavy c'e'");
    assert_eq!(
        (c.model.as_str(), c.degraded, c.rationale.as_str()),
        ("heavy-caro", false, "tier=heavy:auto"),
        "col tier richiesto disponibile NON si degrada, benche' il medium costi 900 volte meno"
    );
}

/// Le funzioni pure del servizio, senza DB: la catena che ogni policy implica.
#[test]
fn la_catena_dipende_dalla_policy_non_dallo_strato() {
    let degrade = ModelRequest::agentic("heavy");
    assert_eq!(
        chain_for(&degrade),
        vec!["heavy", "high", "medium", "light"],
        "Degrade delega al punto unico agentic_tier_chain"
    );
    let exact = ModelRequest::agentic("heavy").tier_policy(TierPolicy::Exact {
        why: ExactReason::ScaleTarget,
    });
    assert_eq!(chain_for(&exact), vec!["heavy"], "Exact non degrada");
    let any = ModelRequest::agentic("heavy").tier_policy(TierPolicy::AnyTier);
    assert!(chain_for(&any).is_empty(), "AnyTier non filtra per tier");
}

/// IL PAVIMENTO (misurato sul campo il 16/07): un modello sotto la soglia non
/// e' un'alternativa peggiore, e' un'alternativa che NON FUNZIONA.
///
/// Il failover enumerava con AnyTier e sceglieva col "tier come indicazione":
/// con openai e anthropic senza credito e' finito su groq/gpt-oss-20b
/// (agentic_index 3.1, il peggiore del parco). Il run non e' fallito — ha
/// prodotto una risposta FUORI TEMA e l'ha dichiarata 'completed'. Un esito
/// bugiardo e' peggio di un fallimento: l'utente ci si fida.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn il_pavimento_scarta_i_modelli_troppo_deboli(pool: PgPool) {
    // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
    sqlx::query("DELETE FROM ai_price_catalog")
        .execute(&pool)
        .await
        .expect("pulizia catalog");
    // Il caso REALE: il 'light' e' il piu' economico, quindi vincerebbe
    // qualunque ordinamento cost-first se il pavimento mancasse.
    sqlx::query(
        "INSERT INTO ai_price_catalog              (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy,               performance_tier, capabilities, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) VALUES              ('groq',     'gpt-oss-20b',       true, true, 'none', 'light',  '[\"code\"]'::jsonb, 0.01, 0.01, 'USD', now()),              ('deepseek', 'deepseek-v4-flash', true, true, 'none', 'medium', '[\"code\"]'::jsonb, 5.00, 5.00, 'USD', now())",
    )
    .execute(&pool)
    .await
    .expect("seed");

    // SENZA pavimento: vince il light (e' 500 volte piu' economico).
    let req = ModelRequest::agentic("")
        .tier_policy(TierPolicy::AnyTier)
        .capability(Some("code"));
    let c = select_model_with_gate(&pool, &req, gate(false)).await.expect("un modello");
    assert_eq!(c.model, "gpt-oss-20b", "senza pavimento vince il piu' economico: e' il difetto");

    // COL pavimento 'medium': il light e' ESCLUSO, anche se costa 500 volte meno.
    let req = ModelRequest::agentic("")
        .tier_policy(TierPolicy::AnyTier)
        .capability(Some("code"))
        .min_tier("medium");
    let c = select_model_with_gate(&pool, &req, gate(false)).await.expect("un modello sopra il pavimento");
    assert_eq!(
        c.model, "deepseek-v4-flash",
        "col pavimento il modello sotto soglia NON e' ammissibile: meglio pagare              500 volte tanto che rispondere fuori tema dichiarando 'completed'"
    );
}

/// Se sotto il pavimento non c'e' NULLA, si fallisce ONESTAMENTE invece di
/// rispondere male. E' la scelta dichiarata: un esito bugiardo e' peggio di un
/// fallimento.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn sotto_il_pavimento_si_fallisce_onestamente(pool: PgPool) {
    // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
    sqlx::query("DELETE FROM ai_price_catalog")
        .execute(&pool)
        .await
        .expect("pulizia catalog");
    sqlx::query(
        "INSERT INTO ai_price_catalog              (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy,               performance_tier, capabilities, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) VALUES              ('groq', 'solo-light', true, true, 'none', 'light', '[\"code\"]'::jsonb, 0.01, 0.01, 'USD', now())",
    )
    .execute(&pool)
    .await
    .expect("seed");
    let req = ModelRequest::agentic("")
        .tier_policy(TierPolicy::AnyTier)
        .capability(Some("code"))
        .min_tier("medium");
    let err = select_model_with_gate(&pool, &req, gate(false)).await.expect_err(
        "sotto il pavimento non c'e' nulla: meglio un fallimento TIPIZZATO che              una risposta inaffidabile spacciata per buona",
    );
    // L'esito e' comunque TIPIZZATO (I6): mai un None muto.
    assert!(!matches!(err, NoModelReason::InvalidRequest(_)), "{err:?}");
}

// ── La scala RELATIVA: tier_from_leader e l'ancora ──────────────────────────

/// Le percentuali del seed (mig 0615).
fn bande_default() -> RelativeBands {
    RelativeBands {
        frontier_pct: 0.85,
        heavy_pct: 0.65,
        high_pct: 0.45,
        medium_pct: 0.20,
    }
}

/// La banda dalle vecchie soglie ASSOLUTE (45/35/25/10, mig 0600, rimosse con
/// la 0614): vive SOLO qui, come termine di paragone del test di
/// comportamento-preservazione.
fn banda_assoluta(idx: f64) -> &'static str {
    if idx >= 45.0 {
        "frontier"
    } else if idx >= 35.0 {
        "heavy"
    } else if idx >= 25.0 {
        "high"
    } else if idx >= 10.0 {
        "medium"
    } else {
        "light"
    }
}

/// La tabella di `tier_from_leader`: bordi inclusivi, leader = frontier per
/// definizione, ancora non positiva = scala indefinita (tutto light).
#[test]
fn tier_from_leader_a_tabella() {
    let b = bande_default();
    for (value, leader, atteso) in [
        (100.0, 100.0, "frontier"), // il leader e' il 100% di se stesso
        (85.0, 100.0, "frontier"),  // bordo inclusivo
        (84.9, 100.0, "heavy"),
        (65.0, 100.0, "heavy"),
        (64.9, 100.0, "high"),
        (45.0, 100.0, "high"),
        (44.9, 100.0, "medium"),
        (20.0, 100.0, "medium"),
        (19.9, 100.0, "light"),
        (0.0, 100.0, "light"),
        (120.0, 100.0, "frontier"), // sopra il leader (ancora in deadband)
        (54.0, 54.0, "frontier"),   // scala ancorata al parco reale
        (10.0, 0.0, "light"),       // ancora non positiva: scala indefinita
    ] {
        assert_eq!(
            tier_from_leader(value, leader, &b),
            atteso,
            "value={value} leader={leader}"
        );
    }
}

/// COMPORTAMENTO-PRESERVAZIONE della Fase A: ad ancora 54.0 (il leader reale,
/// openai/gpt-5.6-sol) le bande relative riproducono le vecchie soglie assolute
/// 45/35/25/10 OVUNQUE tranne che nelle tre finestre di bordo dichiarate, dove
/// le soglie relative (45.9 / 35.1 / 24.3 / 10.8) scavalcano le assolute.
/// Quantificato sul DB vivo (2026-07-19): 5 modelli su 79 cambiano banda.
#[test]
fn ad_ancora_54_la_scala_relativa_preserva_le_bande_assolute() {
    let b = bande_default();
    let dentro_le_finestre_di_bordo = |idx: f64| {
        (45.0..45.9).contains(&idx)   // frontier assoluta, heavy relativa
            || (35.0..35.1).contains(&idx) // heavy assoluta, high relativa
            || (24.3..25.0).contains(&idx) // medium assoluta, high relativa
            || (10.0..10.8).contains(&idx) // medium assoluta, light relativa
    };
    let mut idx = 0.0f64;
    while idx < 60.0 {
        if !dentro_le_finestre_di_bordo(idx) {
            assert_eq!(
                tier_from_leader(idx, 54.0, &b),
                banda_assoluta(idx),
                "indice {idx}: fuori dai bordi dichiarati la banda NON cambia"
            );
        }
        idx += 0.05;
    }
    // I 5 modelli REALI che cambiano banda (query sul catalogo vivo, 19/07):
    // sono tutti dentro le finestre di bordo, e il cambio e' DICHIARATO.
    for (idx, relativa) in [
        (45.7, "heavy"), // x-ai/grok-4.5: era frontier
        (45.6, "heavy"), // openai/gpt-5.6-luna: era frontier
        (24.6, "high"),  // claude-sonnet-4-5: era medium (la relativa lo ALZA)
        (10.6, "light"), // mistral/devstral-2512: era medium
    ] {
        assert_eq!(tier_from_leader(idx, 54.0, &b), relativa, "indice {idx}");
    }
}

/// La DEADBAND dell'ancora: entro il 3% la scala non si muove (anti-flapping),
/// oltre si ri-ancora e la scrittura va persistita. Un'ancora assente o non
/// positiva si fissa subito.
#[test]
fn l_ancora_si_muove_solo_oltre_la_deadband() {
    // Entro la deadband: l'ancora resta quella, niente scrittura.
    assert_eq!(resolve_anchor(Some(54.0), 55.0, 0.03), (54.0, false));
    assert_eq!(resolve_anchor(Some(54.0), 52.5, 0.03), (54.0, false));
    // Oltre (in entrambe le direzioni): nuova ancora, da persistere.
    assert_eq!(resolve_anchor(Some(54.0), 60.0, 0.03), (60.0, true));
    assert_eq!(resolve_anchor(Some(54.0), 44.4, 0.03), (44.4, true));
    // Nessuna ancora precedente (o un fossile non positivo): si fissa subito.
    assert_eq!(resolve_anchor(None, 54.0, 0.03), (54.0, true));
    assert_eq!(resolve_anchor(Some(0.0), 54.0, 0.03), (54.0, true));
}

// ── Scrittura del tier: la precedenza delle fonti ───────────────────────────

/// La tabella di verita' dell'autorita', per intero. E' la regola che prima
/// viveva DUE volte — una WHERE in refresh_tier_prior e un CASE in
/// SQL_QUALIFIED — in due linguaggi diversi, allineate solo dalla diligenza.
#[test]
fn la_precedenza_delle_fonti_e_una_sola_regola() {
    use TierSource::*;
    // Nessuna fonte (colonna NULL): il tier c'e' ma non si sa da dove venga.
    // Chiunque lo rimpiazza — e' cosi' che i 49 fossili del nome declassati
    // dalla mig 0608 tornano correggibili.
    for nuova in [Synced, Measured, Manual] {
        assert!(puo_sovrascrivere(None, nuova), "{nuova:?} su fonte ignota");
    }
    // Ogni fonte corregge se stessa: un sync nuovo aggiorna il sync vecchio,
    // una misura nuova aggiorna la precedente.
    for f in [Synced, Measured, Manual] {
        assert!(puo_sovrascrivere(Some(f), f), "{f:?} su se stessa");
    }
    // Si sale: la misura batte il seme, la curatela batte tutto.
    assert!(puo_sovrascrivere(Some(Synced), Measured));
    assert!(puo_sovrascrivere(Some(Synced), Manual));
    assert!(puo_sovrascrivere(Some(Measured), Manual));
    // NON si scende: il caso che conta. Il sync gira ogni 12h e passerebbe la
    // vita a sovrascrivere le misure e le decisioni dell'admin.
    assert!(!puo_sovrascrivere(Some(Measured), Synced));
    assert!(!puo_sovrascrivere(Some(Manual), Synced));
    assert!(!puo_sovrascrivere(Some(Manual), Measured));
}

/// La proiezione SQL della regola DERIVA dalla regola: non e' una seconda
/// lista da tenere allineata a mano (che e' esattamente com'era prima).
#[test]
fn le_fonti_sovrascrivibili_derivano_dalla_regola() {
    use TierSource::*;
    assert_eq!(fonti_sovrascrivibili(Synced), vec!["", "synced"]);
    assert_eq!(fonti_sovrascrivibili(Measured), vec!["", "synced", "measured"]);
    assert_eq!(
        fonti_sovrascrivibili(Manual),
        vec!["", "synced", "measured", "manual"]
    );
}

/// Il vocabolario e' chiuso e round-trip (regola N): un valore ignoto in
/// colonna vale "nessuna fonte", non un panic ne' un default silenzioso.
#[test]
fn il_vocabolario_delle_fonti_e_chiuso() {
    for f in [TierSource::Synced, TierSource::Measured, TierSource::Manual] {
        assert_eq!(TierSource::parse(Some(f.as_str())), Some(f));
    }
    assert_eq!(TierSource::parse(None), None);
    assert_eq!(TierSource::parse(Some("facts_prior")), None, "vocabolario pre-0608");
    assert_eq!(TierSource::parse(Some("")), None);
}

/// L'autorita' vale sul DB, non solo nella funzione pura: la WHERE ricontrolla
/// la fonte, cosi' una scrittura concorrente non si perde fra la lettura e
/// l'UPDATE (il sync e il worker di qualificazione girano insieme).
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn apply_tier_rispetta_l_autorita_sul_db(pool: PgPool) {
    // Schema REALE (regola O): `tier_source` e' gia' nella migrazione (mig
    // 0608). Il DELETE isola dal catalog di produzione.
    sqlx::query("DELETE FROM ai_price_catalog")
        .execute(&pool)
        .await
        .expect("pulizia catalog");
    sqlx::query(
        "INSERT INTO ai_price_catalog (provider, model, performance_tier, tier_source, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) VALUES \
         ('p', 'fossile',  'medium', NULL, 1.0, 1.0, 'USD', now()), \
         ('p', 'sincro',   'medium', 'synced', 1.0, 1.0, 'USD', now()), \
         ('p', 'misurato', 'medium', 'measured', 1.0, 1.0, 'USD', now()), \
         ('p', 'curato',   'medium', 'manual', 1.0, 1.0, 'USD', now())",
    )
    .execute(&pool)
    .await
    .expect("seed");

    let leggi = |m: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_as::<_, (Option<String>, Option<String>)>(
                "SELECT performance_tier, tier_source FROM ai_price_catalog WHERE model = $1",
            )
            .bind(m)
            .fetch_one(&pool)
            .await
            .expect("riga")
        }
    };

    // Il seme corregge il fossile e se stesso, ma non tocca misura ne' curatela.
    assert!(apply_tier(&pool, "p", "fossile", "heavy", TierSource::Synced)
        .await
        .expect("sql"));
    assert!(apply_tier(&pool, "p", "sincro", "heavy", TierSource::Synced)
        .await
        .expect("sql"));
    assert!(
        !apply_tier(&pool, "p", "misurato", "heavy", TierSource::Synced)
            .await
            .expect("sql"),
        "il sync gira ogni 12h: se sovrascrivesse la misura, la batteria non         servirebbe a nulla"
    );
    assert!(!apply_tier(&pool, "p", "curato", "heavy", TierSource::Synced)
        .await
        .expect("sql"));

    assert_eq!(leggi("fossile").await, (Some("heavy".into()), Some("synced".into())));
    assert_eq!(leggi("sincro").await, (Some("heavy".into()), Some("synced".into())));
    assert_eq!(leggi("misurato").await, (Some("medium".into()), Some("measured".into())));
    assert_eq!(leggi("curato").await, (Some("medium".into()), Some("manual".into())));

    // La misura batte il seme; la curatela batte la misura.
    assert!(apply_tier(&pool, "p", "sincro", "frontier", TierSource::Measured)
        .await
        .expect("sql"));
    assert!(apply_tier(&pool, "p", "misurato", "light", TierSource::Manual)
        .await
        .expect("sql"));
    assert_eq!(leggi("sincro").await, (Some("frontier".into()), Some("measured".into())));
    assert_eq!(leggi("misurato").await, (Some("light".into()), Some("manual".into())));
}

/// Scrivere lo stesso valore dalla stessa fonte non e' una scrittura: evita
/// updated_at che sfarfalla a ogni giro del sync.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn apply_tier_non_scrive_se_nulla_cambia(pool: PgPool) {
    // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
    sqlx::query("DELETE FROM ai_price_catalog")
        .execute(&pool)
        .await
        .expect("pulizia catalog");
    sqlx::query(
        "INSERT INTO ai_price_catalog (provider, model, performance_tier, tier_source, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) \
         VALUES ('p', 'stabile', 'heavy', 'synced', 1.0, 1.0, 'USD', now())",
    )
    .execute(&pool)
    .await
    .expect("seed");
    assert!(
        !apply_tier(&pool, "p", "stabile", "heavy", TierSource::Synced)
            .await
            .expect("sql"),
        "stesso tier, stessa fonte: nessuna riga toccata"
    );
    // Ma se la PROVENIENZA cambia, la riga si scrive anche a tier uguale: un
    // fossile appena convalidato dall'indice deve smettere di dire "non so".
    assert!(
        apply_tier(&pool, "p", "stabile", "heavy", TierSource::Measured)
            .await
            .expect("sql"),
        "stesso tier ma fonte piu' autorevole: la provenienza va registrata"
    );
}

// ── Il pavimento: quando scendere e' un danno, non un ripiego ───────────────

/// IL CASO REALE (misurato sul catalogo il 2026-07-16, riprodotto qui).
///
/// Le figure `medium` del consiglio avevano i loro unici candidati su openai e
/// anthropic; entrambi erano in cooldown per credito esaurito. Con `Degrade` la
/// catena scendeva a `light` e sceglieva il piu' economico rimasto:
/// groq/gpt-oss-20b, agentic_index 3.1 contro il 31.1 del modello atteso. Quel
/// run non falliva — rispondeva FUORI TEMA e si dichiarava `completed`.
///
/// Con `Upgrade` lo stesso parco produce un modello PIU' capace del richiesto:
/// costa di piu', ed e' esattamente cio' che vogliamo pagare.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn col_tier_vuoto_e_niente_sotto_il_pavimento_si_sale(pool: PgPool) {
    // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
    sqlx::query("DELETE FROM ai_price_catalog")
        .execute(&pool)
        .await
        .expect("pulizia catalog");
    sqlx::query(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
          performance_tier, capabilities, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, \
          last_probe_healthy_at) VALUES \
         ('groq',      'gpt-oss-20b', true, true, 'none', 'light',  '[\"chat\"]'::jsonb, 0.075, 0.075, 'USD', now()), \
         ('openrouter','glm-5.2',     true, true, 'none', 'high',   '[\"chat\"]'::jsonb, 0.42, 0.42, 'USD', now()), \
         ('anthropic', 'opus-4-7',    true, true, 'none', 'heavy',  '[\"chat\"]'::jsonb, 5.0, 5.0, 'USD', now())",
    )
    .execute(&pool)
    .await
    .expect("seed");

    // Nessun 'medium' nel parco: e' la situazione del cooldown.
    let req = ModelRequest::agentic("medium")
        .tier_policy(TierPolicy::Flexible)
        .min_tier("medium")
        .capability(Some("chat"));
    let scelto = select_model_with_gate(&pool, &req, gate(false))
        .await
        .expect("il tier superiore ha candidati: si sale");

    assert_eq!(scelto.model, "glm-5.2", "sale al PRIMO tier disponibile (high), non al piu' caro");
    assert_eq!(scelto.effective_tier.as_deref(), Some("high"));
    assert_eq!(scelto.shift, TierShift::Upgraded);
    assert!(!scelto.degraded, "salire NON e' degradare: il flag storico resta falso");
    assert_eq!(scelto.rationale, "tier=medium:upgraded_to=high");
    assert_ne!(
        scelto.model, "gpt-oss-20b",
        "REGRESSIONE: e' il modello che rispondeva fuori tema dichiarandosi         completed. Se questo test fallisce, il consiglio e' tornato a fidarsi         di un agentic_index 3.1"
    );
}

/// Con `Degrade` lo STESSO parco sceglie il light: il discriminante e' la
/// policy, non i dati. E' la prova che il difetto era la policy.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn lo_stesso_parco_con_degrade_cadrebbe_sul_light(pool: PgPool) {
    // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
    sqlx::query("DELETE FROM ai_price_catalog")
        .execute(&pool)
        .await
        .expect("pulizia catalog");
    sqlx::query(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
          performance_tier, capabilities, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, \
          last_probe_healthy_at) VALUES \
         ('groq',      'gpt-oss-20b', true, true, 'none', 'light',  '[\"chat\"]'::jsonb, 0.075, 0.075, 'USD', now()), \
         ('openrouter','glm-5.2',     true, true, 'none', 'high',   '[\"chat\"]'::jsonb, 0.42, 0.42, 'USD', now())",
    )
    .execute(&pool)
    .await
    .expect("seed");
    let req = ModelRequest::agentic("medium").capability(Some("chat"));
    let scelto = select_model_with_gate(&pool, &req, gate(false))
        .await
        .expect("degradando un candidato c'e'");
    assert_eq!(
        scelto.model, "gpt-oss-20b",
        "Degrade scende: e' il comportamento storico, corretto per i turni dove         un modello debole e' meglio di nessuna risposta"
    );
    assert_eq!(scelto.shift, TierShift::Degraded);
    assert_eq!(scelto.rationale, "tier=medium:degraded_to=light");
}

/// Il bersaglio resta il bersaglio: se il tier richiesto ha candidati, non si
/// sale (altrimenti ogni run pagherebbe il tier superiore).
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn con_upgrade_il_tier_richiesto_vince_se_c_e(pool: PgPool) {
    // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
    sqlx::query("DELETE FROM ai_price_catalog")
        .execute(&pool)
        .await
        .expect("pulizia catalog");
    sqlx::query(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
          performance_tier, capabilities, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, \
          last_probe_healthy_at) VALUES \
         ('mistral',  'giusto', true, true, 'none', 'medium', '[\"chat\"]'::jsonb, 1.5, 1.5, 'USD', now()), \
         ('anthropic','caro',   true, true, 'none', 'heavy',  '[\"chat\"]'::jsonb, 5.0, 5.0, 'USD', now())",
    )
    .execute(&pool)
    .await
    .expect("seed");
    let req = ModelRequest::agentic("medium")
        .tier_policy(TierPolicy::Flexible)
        .min_tier("medium")
        .capability(Some("chat"));
    let scelto = select_model_with_gate(&pool, &req, gate(false))
        .await
        .expect("il medium c'e'");
    assert_eq!(scelto.model, "giusto");
    assert_eq!(scelto.shift, TierShift::None);
    assert_eq!(scelto.rationale, "tier=medium:auto");
}

/// Se non c'e' nulla NEMMENO salendo, si fallisce in modo TIPIZZATO (I6): il
/// chiamante convoca chi c'e' o rinuncia, ma nessuno gli passa di nascosto un
/// modello sotto il pavimento.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn se_non_c_e_nulla_sopra_si_fallisce_invece_di_scendere(pool: PgPool) {
    // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
    sqlx::query("DELETE FROM ai_price_catalog")
        .execute(&pool)
        .await
        .expect("pulizia catalog");
    sqlx::query(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
          performance_tier, capabilities, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, \
          last_probe_healthy_at) VALUES \
         ('groq', 'gpt-oss-20b', true, true, 'none', 'light', '[\"chat\"]'::jsonb, 0.075, 0.075, 'USD', now())",
    )
    .execute(&pool)
    .await
    .expect("seed");
    let req = ModelRequest::agentic("heavy")
        .tier_policy(TierPolicy::Flexible)
        .min_tier("heavy")
        .capability(Some("chat"));
    let err = select_model_with_gate(&pool, &req, gate(false))
        .await
        .expect_err("sotto il pavimento c'e' solo un light: non e' un candidato");
    assert!(
        matches!(err, NoModelReason::ChainExhausted { .. }),
        "l'esito e' TIPIZZATO, mai un ripiego silenzioso: {err:?}"
    );
}

/// I DUE INCIDENTI OPPOSTI, conciliati dalla stessa policy. E' il test che
/// impedisce di "risolverne" uno riaprendo l'altro — cosa che e' gia' successa:
/// il fix del 15/07 (degrada, altrimenti il consiglio non si convoca) ha
/// causato il difetto del 16/07 (degrada troppo, fino a un modello che mente).
///
/// Il discriminante non e' la direzione: e' il PAVIMENTO.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn il_pavimento_concilia_i_due_incidenti_opposti(pool: PgPool) {
    // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
    sqlx::query("DELETE FROM ai_price_catalog")
        .execute(&pool)
        .await
        .expect("pulizia catalog");
    sqlx::query(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
          performance_tier, capabilities, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, \
          last_probe_healthy_at) VALUES \
         ('groq',    'gpt-oss-20b',     true, true, 'none', 'light', '[\"chat\"]'::jsonb, 0.075, 0.075, 'USD', now()), \
         ('deepseek','deepseek-v4-pro', true, true, 'none', 'high',  '[\"chat\"]'::jsonb, 0.5, 0.5, 'USD', now())",
    )
    .execute(&pool)
    .await
    .expect("seed");

    // 15/07 — purpose 'heavy', gli heavy sono spariti (cooldown). Il modello
    // sano sta UN gradino sotto e vale 36.4 di agentic_index: si DEVE scendere,
    // altrimenti il consiglio non si convoca affatto.
    let heavy = ModelRequest::agentic("heavy")
        .tier_policy(TierPolicy::Flexible)
        .min_tier("medium")
        .capability(Some("chat"));
    let scelto = select_model_with_gate(&pool, &heavy, gate(false))
        .await
        .expect("scendere di un gradino e' ammesso: sopra il pavimento");
    assert_eq!(scelto.model, "deepseek-v4-pro");
    assert_eq!(scelto.shift, TierShift::Degraded, "e' una degradazione, e va bene");

    // 16/07 — purpose 'medium', i medium sono spariti (stesso cooldown). Sotto
    // c'e' solo il light che mente: NON si scende, si sale.
    let medium = ModelRequest::agentic("medium")
        .tier_policy(TierPolicy::Flexible)
        .min_tier("medium")
        .capability(Some("chat"));
    let scelto = select_model_with_gate(&pool, &medium, gate(false))
        .await
        .expect("sopra il pavimento c'e' un high: si sale");
    assert_eq!(
        scelto.model, "deepseek-v4-pro",
        "REGRESSIONE: se qui compare gpt-oss-20b, il consiglio e' tornato a         fidarsi di un agentic_index 3.1 che si dichiara completed"
    );
    assert_eq!(scelto.shift, TierShift::Upgraded);
}

/// `Flexible` senza pavimento e' una degradazione senza freni con un nome
/// rassicurante: il servizio la RIFIUTA (I8) invece di scegliere un default
/// implicito, che e' come nascono i difetti di questo modulo.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn flexible_senza_pavimento_e_una_richiesta_invalida(pool: PgPool) {
    let req = ModelRequest {
        min_tier: None,
        ..ModelRequest::agentic("medium").tier_policy(TierPolicy::Flexible)
    };
    let err = select_model_with_gate(&pool, &req, gate(false))
        .await
        .expect_err("senza pavimento la policy non ha significato");
    assert!(matches!(err, NoModelReason::InvalidRequest(_)), "{err:?}");
}

/// La catena di `Flexible`: bersaglio, poi giu' FINO al pavimento, poi su.
/// Nessun tier sotto il pavimento vi compare mai, per nessun bersaglio.
#[test]
fn la_catena_flexible_non_scende_mai_sotto_il_pavimento() {
    for tier in ["light", "medium", "high", "heavy", "frontier"] {
        let req = ModelRequest::agentic(tier)
            .tier_policy(TierPolicy::Flexible)
            .min_tier("medium");
        let chain = chain_for(&req);
        assert!(
            chain.iter().all(|t| tier_rank(t) >= tier_rank("medium")),
            "bersaglio {tier}: la catena {chain:?} scende sotto il pavimento"
        );
        assert_eq!(chain.first(), Some(&tier).filter(|t| tier_rank(t) >= 2).or(Some(&"medium")),
            "il bersaglio (o il pavimento, se il bersaglio e' sotto) va provato per primo: {chain:?}");
        // Nessun duplicato: la coda ascendente non ripete cio' che la discendente
        // ha gia' incluso.
        let mut visti = chain.clone();
        visti.sort_unstable();
        visti.dedup();
        assert_eq!(visti.len(), chain.len(), "catena con duplicati: {chain:?}");
    }
    // Bersaglio heavy, pavimento medium: giu' fino a medium, poi frontier.
    let req = ModelRequest::agentic("heavy")
        .tier_policy(TierPolicy::Flexible)
        .min_tier("medium");
    assert_eq!(chain_for(&req), vec!["heavy", "high", "medium", "frontier"]);
}

// ── Rank::CostFirst cache-aware e finestre-aware (mig 0721) ─────────────────

/// Il flag di rollout, scritto e RESO VISIBILE: la cache dei settings (TTL 60s,
/// per pool) servirebbe altrimenti il valore vecchio per un minuto, e il test
/// misurerebbe lo stato di prima della flip.
async fn accendi_cost_rank(pool: &PgPool, acceso: bool) {
    sqlx::query("UPDATE settings SET value = $1 WHERE key = $2")
        .bind(if acceso { "true" } else { "false" })
        .bind(super::super::cost_rank::FLAG_CACHE_AWARE)
        .execute(pool)
        .await
        .expect("flip flag cost_rank");
    nexus_auth::invalidate_setting_cache(pool, super::super::cost_rank::FLAG_CACHE_AWARE);
}

/// Un modello del catalog con le colonne che il criterio cache-aware legge:
/// listino di input, tariffa di cache, tier.
async fn seed_cost_rank_modello(
    pool: &PgPool,
    provider: &str,
    model: &str,
    tier: &str,
    input: f64,
    cache_read: Option<f64>,
) {
    sqlx::query(
        "INSERT INTO ai_price_catalog \
           (provider, model, performance_tier, input_cost_per_million_tokens, \
            output_cost_per_million_tokens, cache_read_cost_per_million_tokens, \
            currency, is_enabled, supports_tool_use, agentic_thinking_policy, \
            capabilities, qualified_capabilities, context_window, pricing_state, \
            qualification_state, qualification_expires_at, last_probe_healthy_at) \
         VALUES ($1,$2,$3,$4,1.0,$5,'USD',TRUE,TRUE,'none','[\"code\"]'::jsonb, \
                 '[\"code\"]'::jsonb,200000,'priced','qualified', \
                 now() + interval '30 days',now())",
    )
    .bind(provider)
    .bind(model)
    .bind(tier)
    .bind(input)
    .bind(cache_read)
    .execute(pool)
    .await
    .expect("seed catalog cost_rank");
}

/// La dichiarazione `supports_prompt_cache` nella tabella che alimenta la
/// vista unica `v_model_capabilities` (ADR 0024).
async fn dichiara_prompt_cache(pool: &PgPool, provider: &str, model: &str, flag: bool) {
    sqlx::query(
        "INSERT INTO nexus_provider_capabilities (provider, model, supports_prompt_cache) \
         VALUES ($1, $2, $3)",
    )
    .bind(provider)
    .bind(model)
    .bind(flag)
    .execute(pool)
    .await
    .expect("dichiarazione prompt cache");
}

/// Righe di ledger scritte dal PRODUTTORE reale (`record_tokens`) con le FK
/// soddisfatte davvero (regola O: il seed a mano del ledger e' esattamente
/// il precedente "la fixture fissava l'assunto"). Stesso pattern dei test di
/// escalation_port.
async fn seed_hit_ledger(
    pool: &PgPool,
    provider: &str,
    model: &str,
    righe: usize,
    prompt: i64,
    cache: i64,
) {
    let team = uuid::Uuid::new_v4();
    let user = uuid::Uuid::new_v4();
    let project = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ($1,'T',$2)")
        .bind(team)
        .bind(team.to_string())
        .execute(pool)
        .await
        .expect("team");
    sqlx::query("INSERT INTO users (id, email, display_name) VALUES ($1,$2,'U')")
        .bind(user)
        .bind(format!("{user}@t.local"))
        .execute(pool)
        .await
        .expect("user");
    sqlx::query(
        "INSERT INTO projects (id, team_id, name, slug, owner_user_id) \
         VALUES ($1,$2,'P',$3,$4)",
    )
    .bind(project)
    .bind(team)
    .bind(project.to_string())
    .bind(user)
    .execute(pool)
    .await
    .expect("project");

    let id = nexus_ledger::Identity {
        user_id: user,
        project_id: project,
    };
    for _ in 0..righe {
        let usage = nexus_pricing::TokenUsage {
            prompt_tokens: prompt,
            completion_tokens: 0,
            cache_read_tokens: cache,
            cache_creation_tokens: 0,
        };
        nexus_ledger::record_tokens(pool, id, provider, model, &usage, None, "", "test")
            .await
            .expect("record_tokens");
    }
}

/// Test 1 del design (Fase 3, Lotto 1): CostFirst ordina sul costo ATTESO, non
/// sul listino nominale. A costa 0.40 senza cache; B costa 0.60 di listino ma
/// con cache_read 0.06 e hit misurato 70% sul ledger il suo costo atteso e'
/// 0.60*0.3 + 0.06*0.7 = 0.222 < 0.40: col flag acceso vince B.
///
/// MUTAZIONE (eseguita davvero, vedi commit): se il ranking torna al listino
/// nominale — p.es. `risolvi_hit` che scarta la misura, o l'innesto rimosso —
/// il test rosseggia mostrando A.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn costfirst_ordina_sul_costo_atteso_non_sul_listino(pool: PgPool) {
    sqlx::query("DELETE FROM ai_price_catalog")
        .execute(&pool)
        .await
        .expect("pulizia catalog");
    seed_cost_rank_modello(&pool, "prov-a", "a-listino", "medium", 0.40, None).await;
    seed_cost_rank_modello(&pool, "prov-b", "b-cache", "medium", 0.60, Some(0.06)).await;
    dichiara_prompt_cache(&pool, "prov-b", "b-cache", true).await;
    // 25 righe finalized (>= soglia 20 della mig 0656) con hit 70%.
    seed_hit_ledger(&pool, "prov-b", "b-cache", 25, 1_000, 700).await;

    let req = ModelRequest::agentic("medium").capability(Some("code"));

    // Flag OFF (default della mig 0721): comanda il listino nominale, vince A.
    let c = select_model_with_gate(&pool, &req, gate(false))
        .await
        .expect("un modello");
    assert_eq!(c.model, "a-listino", "a flag OFF comanda il listino nominale");

    // Flag ON: comanda il costo atteso, vince B.
    accendi_cost_rank(&pool, true).await;
    let c = select_model_with_gate(&pool, &req, gate(false))
        .await
        .expect("un modello");
    assert_eq!(
        c.model, "b-cache",
        "costo atteso 0.222 < 0.40: se qui c'e' ancora 'a-listino' il ranking \
         e' tornato al listino nominale"
    );
}

/// Test 3 del design: `supports_prompt_cache` e' il discriminante. Con la
/// dichiarazione FALSE ma il ledger che misura hit > 0 (deriva della colonna)
/// vince la MISURA — il fatto piu' recente batte la dichiarazione stantia,
/// stessa evidenza della mig 0703. Le etichette dei casi senza misura
/// (Observed(0.0) dichiarata contro Unknown) sono provate a tabella nel test
/// puro di cost_rank.
///
/// MUTAZIONE: se la dichiarazione FALSE azzerasse la misura (precedenza
/// invertita), il costo atteso di B tornerebbe 0.60 e vincerebbe A.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn senza_cache_dichiarata_niente_sconto_ma_la_misura_vince(pool: PgPool) {
    sqlx::query("DELETE FROM ai_price_catalog")
        .execute(&pool)
        .await
        .expect("pulizia catalog");
    seed_cost_rank_modello(&pool, "prov-a", "a-listino", "medium", 0.40, None).await;
    seed_cost_rank_modello(&pool, "prov-b", "b-smentito", "medium", 0.60, Some(0.06)).await;
    // La colonna dice FALSE, il ledger misura il contrario.
    dichiara_prompt_cache(&pool, "prov-b", "b-smentito", false).await;
    seed_hit_ledger(&pool, "prov-b", "b-smentito", 25, 1_000, 700).await;
    accendi_cost_rank(&pool, true).await;

    let req = ModelRequest::agentic("medium").capability(Some("code"));
    let c = select_model_with_gate(&pool, &req, gate(false))
        .await
        .expect("un modello");
    assert_eq!(
        c.model, "b-smentito",
        "il ledger misura hit 70%: la misura vince sulla dichiarazione FALSE \
         (deriva della colonna, con warn)"
    );
}

/// Test 4 del design: sotto `min_samples` l'hit e' IGNOTO e il costo atteso
/// resta il listino pieno — parita' col comportamento di oggi. B ha hit
/// altissimo ma solo 5 campioni (soglia 20, mig 0656): non conta.
///
/// MUTAZIONE: se la soglia sparisse (o l'ignoto degradasse a "scontato"),
/// B vincerebbe con 5 campioni e il test rosseggerebbe mostrando B.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn hit_ignoto_resta_listino_pieno(pool: PgPool) {
    sqlx::query("DELETE FROM ai_price_catalog")
        .execute(&pool)
        .await
        .expect("pulizia catalog");
    seed_cost_rank_modello(&pool, "prov-a", "a-listino", "medium", 0.40, None).await;
    seed_cost_rank_modello(&pool, "prov-b", "b-pochi-campioni", "medium", 0.60, Some(0.06)).await;
    dichiara_prompt_cache(&pool, "prov-b", "b-pochi-campioni", true).await;
    seed_hit_ledger(&pool, "prov-b", "b-pochi-campioni", 5, 1_000, 900).await;
    accendi_cost_rank(&pool, true).await;

    let req = ModelRequest::agentic("medium").capability(Some("code"));
    let c = select_model_with_gate(&pool, &req, gate(false))
        .await
        .expect("un modello");
    assert_eq!(
        c.model, "a-listino",
        "5 campioni sotto la soglia di 20: hit ignoto, listino pieno, vince il \
         nominale piu' basso"
    );
}

/// Test 5 del design: a ledger VUOTO flag OFF e flag ON scelgono lo stesso
/// modello — il riordino senza misure e' un'identita' (sort stabile, hit
/// Unknown per tutti, stesso asse del listino).
///
/// MUTAZIONE: se il riordino a hit ignoto alterasse l'ordine (p.es. un sort
/// instabile, o un default di hit inventato), le due scelte divergerebbero.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn flag_off_bit_identico(pool: PgPool) {
    sqlx::query("DELETE FROM ai_price_catalog")
        .execute(&pool)
        .await
        .expect("pulizia catalog");
    seed_cost_rank_modello(&pool, "prov-a", "a-economico", "medium", 0.40, None).await;
    seed_cost_rank_modello(&pool, "prov-b", "b-caro", "medium", 0.60, Some(0.06)).await;
    dichiara_prompt_cache(&pool, "prov-b", "b-caro", true).await;

    let req = ModelRequest::agentic("medium").capability(Some("code"));
    let off = select_model_with_gate(&pool, &req, gate(false))
        .await
        .expect("scelta a flag OFF");
    accendi_cost_rank(&pool, true).await;
    let on = select_model_with_gate(&pool, &req, gate(false))
        .await
        .expect("scelta a flag ON");
    assert_eq!(
        (off.provider, off.model),
        (on.provider, on.model),
        "a ledger vuoto il riordino non deve cambiare la scelta"
    );
}

/// Test 6 del design: nel fan-out multi-tier il riordino cambia l'ordine
/// DENTRO ogni gruppo di tier, mai fra gruppi. `m1-economico` (medium) e' il
/// piu' economico in assoluto ma non scavalca i due heavy; fra gli heavy la
/// cache efficace di `h1-cache` (10.0 nominale, atteso 3.7) batte il nominale
/// di `h2-nominale` (5.0).
///
/// MUTAZIONE: se il reranker ordinasse globalmente sul costo, `m1-economico`
/// finirebbe primo e il test rosseggerebbe.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn il_riordino_non_scavalca_il_tier(pool: PgPool) {
    sqlx::query("DELETE FROM ai_price_catalog")
        .execute(&pool)
        .await
        .expect("pulizia catalog");
    // Due heavy dello stesso provider: il fan-out (min_distinct=2) deve
    // scendere la catena fino al medium per il secondo provider.
    seed_cost_rank_modello(&pool, "prov-uno", "h2-nominale", "heavy", 5.0, None).await;
    seed_cost_rank_modello(&pool, "prov-uno", "h1-cache", "heavy", 10.0, Some(1.0)).await;
    seed_cost_rank_modello(&pool, "prov-due", "m1-economico", "medium", 0.1, None).await;
    dichiara_prompt_cache(&pool, "prov-uno", "h1-cache", true).await;
    // h1: hit 70% -> atteso 10.0*0.3 + 1.0*0.7 = 3.7 < 5.0 di h2.
    seed_hit_ledger(&pool, "prov-uno", "h1-cache", 25, 1_000, 700).await;
    accendi_cost_rank(&pool, true).await;

    let req = ModelRequest::agentic("heavy").capability(Some("code"));
    let scelte = select_models(&pool, &req, 5, 2).await.expect("fan-out");
    let modelli: Vec<&str> = scelte.iter().map(|c| c.model.as_str()).collect();
    assert_eq!(
        modelli,
        vec!["h1-cache", "h2-nominale", "m1-economico"],
        "dentro il gruppo heavy comanda il costo atteso (h1 prima di h2); il \
         medium resta DOPO gli heavy anche se costa 37 volte meno"
    );
}
