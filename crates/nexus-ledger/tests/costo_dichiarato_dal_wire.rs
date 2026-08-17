//! Il costo DICHIARATO dal fornitore vince sul riprezzato.
//!
//! OpenRouter (usage accounting, mig 0717) dichiara in `usage.cost` il costo
//! esatto della chiamata: e' la fattura del fornitore, non una stima. Il caso
//! che ha motivato il lavoro e' il modello openrouter entrato dalla discovery
//! (`pricing_state='unknown'`, listino a 0): OGNI sua chiamata entrava nel
//! ledger a $0.000, e la spesa reale verso il fornitore era invisibile.
//!
//! I test attraversano `record_tokens`/`record_discarded` (i produttori reali
//! delle righe, regola O) e rileggono le colonne dallo schema vero applicato
//! dal META_MIGRATOR.

use nexus_ledger::{CostoDichiarato, Identity};
use nexus_pricing::TokenUsage;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// L'identita' che le FK del ledger esigono, dal seeder unico dello schema META.
async fn identita(pool: &PgPool) -> Identity {
    let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(pool).await;
    Identity {
        user_id,
        project_id,
    }
}

/// Listino NOTO con tariffe semplici: 1M di prompt a 1.0 = riprezzato 1.0.
async fn seed_listino_noto(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO ai_price_catalog ( \
             provider, model, \
             input_cost_per_million_tokens, output_cost_per_million_tokens, \
             currency, pricing_state \
         ) VALUES ('openrouter', 'qwen/qwen3-235b-a22b-2507', 1.0, 0.0, 'USD', 'priced')",
    )
    .execute(pool)
    .await
    .expect("seed listino noto");
}

/// Il caso reale: modello entrato dalla discovery, listino a 0 e stato
/// `unknown` (la forma di `insert_new_chat_model`). UPSERT perche' la riga di
/// `z-ai/glm-4.7-flash` esiste gia' nel seed delle migrazioni: qui la si
/// riporta esattamente allo stato in cui la discovery la lascia.
async fn seed_listino_unknown(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO ai_price_catalog ( \
             provider, model, \
             input_cost_per_million_tokens, output_cost_per_million_tokens, \
             currency, pricing_state \
         ) VALUES ('openrouter', 'z-ai/glm-4.7-flash', 0.0, 0.0, 'USD', 'unknown') \
         ON CONFLICT ON CONSTRAINT uq_price_catalog_provider_model DO UPDATE \
            SET input_cost_per_million_tokens = 0.0, \
                output_cost_per_million_tokens = 0.0, \
                cache_read_cost_per_million_tokens = NULL, \
                cache_creation_cost_per_million_tokens = NULL, \
                currency = 'USD', \
                pricing_state = 'unknown'",
    )
    .execute(pool)
    .await
    .expect("seed listino unknown");
}

/// La riga di un run, con le colonne su cui la precedenza decide.
async fn riga_del_run(pool: &PgPool, run: Uuid) -> sqlx::postgres::PgRow {
    sqlx::query(
        "SELECT total_cost::float8 AS total_cost, input_cost::float8 AS input_cost, details \
           FROM ai_usage_ledger WHERE run_id = $1",
    )
    .bind(run)
    .fetch_one(pool)
    .await
    .expect("la riga del run deve esistere")
}

/// PRECEDENZA: col listino NOTO (riprezzato 1.0) e un dichiarato 0.5, la riga
/// porta 0.5 come totale, la provenienza nel campo (`cost_source`) e il
/// riprezzato conservato per l'audit; senza dichiarato resta il listino.
///
/// MUTAZIONE dichiarata: invertendo la precedenza in `applica_costo_dichiarato`
/// (il riprezzato vince) il primo assert cade con 1.0 — il valore del difetto,
/// cioe' il costo del listino al posto della fattura del fornitore. Togliendo
/// `repriced_total_cost` dai details, cade l'assert sull'audit.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn il_dichiarato_vince_sul_riprezzato(pool: PgPool) {
    let id = identita(&pool).await;
    seed_listino_noto(&pool).await;
    let tokens = TokenUsage::senza_cache(1_000_000, 0);

    // Con dichiarato: la fattura del fornitore.
    let run_dichiarato = Uuid::new_v4();
    let entry = nexus_ledger::record_tokens(
        &pool,
        id,
        "openrouter",
        "qwen/qwen3-235b-a22b-2507",
        &tokens,
        Some(CostoDichiarato {
            total_usd: 0.5,
            upstream_usd: Some(0.4),
        }),
        &run_dichiarato.to_string(),
        "chat",
        None,
    )
    .await
    .expect("riga scritta");
    assert!(
        (entry.total_cost - 0.5).abs() < 1e-9,
        "la dichiarazione sulla risposta deve portare il costo VERO: {}",
        entry.total_cost
    );

    let riga = riga_del_run(&pool, run_dichiarato).await;
    assert!(
        (riga.get::<f64, _>("total_cost") - 0.5).abs() < 1e-9,
        "totale atteso 0.5 (dichiarato), letto {}",
        riga.get::<f64, _>("total_cost")
    );
    // Le COMPONENTI restano il riprezzato indicativo: non si ripartisce il
    // dichiarato su numeri che il fornitore non ha dichiarato.
    assert!((riga.get::<f64, _>("input_cost") - 1.0).abs() < 1e-9);
    let details: serde_json::Value = riga.get("details");
    assert_eq!(details["cost_source"], "provider_declared");
    assert_eq!(details["repriced_total_cost"], 1.0);
    assert_eq!(details["upstream_inference_cost"], 0.4);

    // Senza dichiarato: il listino, come sempre, e la provenienza lo dice.
    let run_riprezzato = Uuid::new_v4();
    nexus_ledger::record_tokens(
        &pool,
        id,
        "openrouter",
        "qwen/qwen3-235b-a22b-2507",
        &tokens,
        None,
        &run_riprezzato.to_string(),
        "chat",
        None,
    )
    .await
    .expect("riga scritta");
    let riga = riga_del_run(&pool, run_riprezzato).await;
    assert!((riga.get::<f64, _>("total_cost") - 1.0).abs() < 1e-9);
    let details: serde_json::Value = riga.get("details");
    assert_eq!(details["cost_source"], "repriced");
    assert!(details.get("repriced_total_cost").is_none());
}

/// IL caso misurato: modello openrouter da discovery (`pricing_state='unknown'`,
/// listino 0). Il dichiarato e' l'UNICO costo vero: la riga deve portarlo, non
/// lo $0.000 di prima. Vale per la riga finalized E per la discarded (uno
/// scarto degenere porta lo stesso usage del wire, costo compreso).
///
/// MUTAZIONE: facendo applicare il dichiarato solo su `PriceLookup::Priced`
/// (cioe' saltandolo dove il listino e' ignoto) entrambi gli assert sul totale
/// cadono con 0.0 — la forma esatta del difetto.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn su_listino_ignoto_il_dichiarato_e_lunico_costo_vero(pool: PgPool) {
    let id = identita(&pool).await;
    seed_listino_unknown(&pool).await;
    let tokens = TokenUsage::senza_cache(50_000, 1_000);

    let run = Uuid::new_v4();
    let entry = nexus_ledger::record_tokens(
        &pool,
        id,
        "openrouter",
        "z-ai/glm-4.7-flash",
        &tokens,
        Some(CostoDichiarato {
            total_usd: 0.0021,
            upstream_usd: None,
        }),
        &run.to_string(),
        "chat",
        None,
    )
    .await
    .expect("riga scritta");
    assert!(
        (entry.total_cost - 0.0021).abs() < 1e-12,
        "listino unknown + dichiarato: il totale e' la fattura, non 0 ({})",
        entry.total_cost
    );
    let riga = riga_del_run(&pool, run).await;
    assert!((riga.get::<f64, _>("total_cost") - 0.0021).abs() < 1e-12);
    let details: serde_json::Value = riga.get("details");
    assert_eq!(details["cost_source"], "provider_declared");
    // `price_state` risponde a un'ALTRA domanda e resta quello del listino.
    assert_eq!(details["price_state"], "unknown");

    // Lo scarto degenere con usage e costo dal wire: stessa precedenza.
    nexus_ledger::record_discarded(
        &pool,
        Some(id),
        "openrouter",
        "z-ai/glm-4.7-flash",
        nexus_ledger::DiscardReason::DegenerateHollow,
        Some(&tokens),
        Some(CostoDichiarato {
            total_usd: 0.0007,
            upstream_usd: None,
        }),
        &run.to_string(),
        "chat",
    )
    .await;
    let scarto = sqlx::query(
        "SELECT total_cost::float8 AS total_cost, details FROM ai_usage_ledger \
          WHERE status = 'discarded'",
    )
    .fetch_one(&pool)
    .await
    .expect("riga discarded");
    assert!(
        (scarto.get::<f64, _>("total_cost") - 0.0007).abs() < 1e-12,
        "anche lo scarto misura la spesa VERA: {}",
        scarto.get::<f64, _>("total_cost")
    );
    let details: serde_json::Value = scarto.get("details");
    assert_eq!(details["cost_source"], "provider_declared");
}

// Il ramo "piattaforma NON in USD" (il dichiarato non si applica e lo dice
// con `declared_cost_skipped`) e' coperto dal test unitario del criterio puro
// in `scrittura.rs::tests::fuori_da_usd_il_dichiarato_non_si_applica`:
// esercitarlo da qui richiederebbe di riscrivere `billing_base_currency`, che
// e' territorio di nexus-pricing (guard `pricing-single-source`).
