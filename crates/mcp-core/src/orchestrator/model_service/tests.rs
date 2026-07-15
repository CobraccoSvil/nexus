//! Il test PARAMETRICO del servizio unico.
//!
//! Il punto non e' coprire qualche caso: e' che il costo di aggiungere un filtro
//! di eleggibilita' diventi "una riga nel servizio, ZERO nei test" — la matrice
//! lo verifica gia' su tutte le modalita'. E' l'opposto di com'e' andata finora:
//! il gate di qualificazione e' stato aggiunto a mano call site per call site, e
//! due siti (`route_by_slots`, `select_models_for_requirement`) sono rimasti
//! indietro senza che nulla arrossisse.

use super::*;
use crate::test_support::create_ai_price_catalog_table;

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
    create_ai_price_catalog_table(pool).await;
    // Il modello SANO: l'unica scelta ammissibile.
    sqlx::query(&format!(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
          performance_tier, capabilities, qualification_state, qualification_expires_at, \
          input_cost_per_million_tokens, context_window) VALUES \
         ('sano-provider', 'modello-sano', true, true, 'none', '{tier}', \
          '[\"reasoning\"]'::jsonb, 'qualified', now() + interval '30 days', 1.0, 200000)"
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
              input_cost_per_million_tokens, context_window) VALUES \
             ('veleno-provider', '{}', true, true, 'none', '{tier}', \
              '[\"reasoning\"]'::jsonb, 'qualified', now() + interval '30 days', 0.01, 999999)",
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
                Rank::HighestTierFirst,
            ] {
                v.push((policy, profile, rank));
            }
        }
    }
    v
}

/// I1/I2: NESSUNA modalita' sceglie un modello non eleggibile. E' il test che
/// rende il servizio un punto unico invece di una convenzione.
#[sqlx::test]
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
            sqlx::query("DROP TABLE ai_price_catalog")
                .execute(&pool)
                .await
                .expect("drop");
        }
    }
}

/// I3: la degradazione e' MONOTONA — mai un tier PIU' capace del richiesto.
/// E con `Exact` il tier effettivo E' quello richiesto, sempre.
#[sqlx::test]
async fn degradazione_monotona_in_ogni_modalita(pool: PgPool) {
    create_ai_price_catalog_table(&pool).await;
    // Solo un 'medium' sano: chi chiede 'heavy' puo' solo degradare (o fallire).
    sqlx::query(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
          performance_tier, capabilities, input_cost_per_million_tokens) VALUES \
         ('p', 'solo-medium', true, true, 'none', 'medium', '[\"reasoning\"]'::jsonb, 1.0)",
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
#[sqlx::test]
async fn il_pin_non_degrada_mai(pool: PgPool) {
    create_ai_price_catalog_table(&pool).await;
    sqlx::query(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
          performance_tier, capabilities, input_cost_per_million_tokens) VALUES \
         ('altro',   'altro-medium',  true, true, 'none', 'medium', '[\"code\"]'::jsonb, 1.0), \
         ('pinnato', 'pinnato-light', true, true, 'none', 'light',  '[\"code\"]'::jsonb, 0.5)",
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
#[sqlx::test]
async fn il_gate_che_svuota_il_pool_si_distingue_dal_parco_fermo(pool: PgPool) {
    create_ai_price_catalog_table(&pool).await;
    sqlx::query("CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .execute(&pool)
        .await
        .expect("settings");
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES \
         ('agent.model_qualification.enforce_routing_gate', 'true')",
    )
    .execute(&pool)
    .await
    .expect("gate on");
    // Un modello SANO ma NON qualificato: col gate acceso il pool e' vuoto, ma il
    // parco NON e' fermo. Il motivo deve dirlo.
    sqlx::query(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
          performance_tier, capabilities, qualification_state, input_cost_per_million_tokens) VALUES \
         ('p', 'sano-ma-non-qualificato', true, true, 'none', 'medium', \
          '[\"code\"]'::jsonb, 'unqualified', 1.0)",
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

/// I7: `HighestTierFirst` ordina per capacita' REALE su tutti e 5 i livelli.
/// E' il test che `agent_run.rs:3525` non ha mai avuto, ed e' il motivo per cui
/// una scala a 3 livelli e' sopravvissuta li' per mesi.
#[sqlx::test]
async fn highest_tier_first_mette_il_frontier_sopra_il_medium(pool: PgPool) {
    create_ai_price_catalog_table(&pool).await;
    // Il frontier e' il PIU' ECONOMICO: se l'ordinamento per tier non funzionasse,
    // il tie-break sul costo lo mascherebbe. (E' esattamente cio' che rendeva
    // silenzioso il difetto reale: `costo DESC` pescava i modelli cari.)
    sqlx::query(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
          performance_tier, capabilities, input_cost_per_million_tokens) VALUES \
         ('p', 'il-frontier', true, true, 'none', 'frontier', '[\"code\"]'::jsonb, 1.0), \
         ('p', 'il-medium',   true, true, 'none', 'medium',   '[\"code\"]'::jsonb, 50.0), \
         ('p', 'il-light',    true, true, 'none', 'light',    '[\"code\"]'::jsonb, 99.0)",
    )
    .execute(&pool)
    .await
    .expect("seed");

    let req = ModelRequest::agentic("heavy")
        .capability(Some("code"))
        .tier_policy(TierPolicy::AnyTier)
        .rank(Rank::HighestTierFirst);
    let c = select_model_with_gate(&pool, &req, gate(false)).await.expect("c'e' un modello");
    assert_eq!(
        c.model, "il-frontier",
        "'sali al piu' capace' deve scegliere il FRONTIER, non un medium: col CASE \
         a 3 livelli frontier e high collassavano su light e questa asserzione era \
         rossa sul codice reale"
    );
}

/// I4: `degraded` e' coerente col tier effettivo, e NON scatta quando il tier
/// richiesto e' disponibile (niente ripieghi gratuiti).
#[sqlx::test]
async fn degraded_non_scatta_se_il_tier_richiesto_c_e(pool: PgPool) {
    create_ai_price_catalog_table(&pool).await;
    sqlx::query(
        "INSERT INTO ai_price_catalog \
         (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
          performance_tier, capabilities, input_cost_per_million_tokens) VALUES \
         ('p', 'heavy-caro',      true, true, 'none', 'heavy',  '[\"code\"]'::jsonb, 90.0), \
         ('p', 'medium-economico', true, true, 'none', 'medium', '[\"code\"]'::jsonb, 0.1)",
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

/// `Rank::HighestTierFirst` deriva la scala dal vocabolario unico: nessun CASE
/// scritto a mano puo' rientrare da qui.
#[test]
fn il_rank_per_tier_viene_dal_vocabolario_unico() {
    let sql = Rank::HighestTierFirst.to_sql();
    for t in nexus_agent_graph::decisions::tiers::PERFORMANCE_TIERS {
        assert!(
            sql.contains(&format!("WHEN '{t}' THEN {}", tier_rank(t))),
            "il livello '{t}' manca nell'ordinamento per tier: {sql}"
        );
    }
}
