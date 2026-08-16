//! La GIUNTURA della finalizzazione, e la vista che legge quelle colonne.
//!
//! Questi quattro test vivevano in `mcp-core::billing` e sono andati persi
//! nell'estrazione del crate: il modulo che li ospitava e' stato riscritto come
//! adapter e con lui sono spariti. Nessuno se n'era accorto, perche' il gate
//! guarda cio' che c'e' e non cio' che manca — ed e' esattamente il tipo di buco
//! che questo crate esiste per chiudere.
//!
//! Cio' che coprono, e che nient'altro copre:
//!
//! 1. **La giuntura bind -> colonna della UPDATE di finalizzazione.** I test
//!    testuali accanto alla SQL (`le_due_scritture_nominano_le_colonne_di_cache`,
//!    `i_segnaposto_coprono_i_bind`) dimostrano che i nomi compaiono e che i
//!    segnaposto tornano; non possono dimostrare che il valore bindato in
//!    posizione N finisca nella colonna che in posizione N e' dichiarata. Con
//!    quattro conteggi e cinque importi adiacenti e omogenei, uno scambio non e'
//!    un errore che il compilatore veda, non e' un errore SQL, e si paga in
//!    denaro. L'unico modo di dimostrarlo e' rileggere la riga dal database vero.
//!
//! 2. **La vista `ai_usage_analytics_view` (mig 0644).** Nessun altro test la
//!    interroga. La sua premessa e' la convenzione del ledger — `prompt_tokens`
//!    e' il LORDO e i due conteggi di cache ne sono sottoinsiemi — e premessa e
//!    codice possono divergere in silenzio: la 0405 calcolava
//!    `input_tokens_gross = prompt + cache_read`, che col prompt lordo conta i
//!    cache_read DUE volte e sottostima il riuso.
//!
//! I token qui si costruiscono direttamente come [`TokenUsage`], che e' il
//! contratto d'ingresso del crate. La conversione dal wire del gateway a quel
//! tipo ha i suoi test dove vive il produttore: `usage_dal_wire_porta_le_quattro_quantita`
//! in `mcp-core::billing` (dal JSON del wire) e `i_token_di_cache_arrivano_alla_riga_di_ledger`
//! in `nexus-gateway::server::billing` (da `LlmUsage::normalized`).

use nexus_ledger::{ChargedBy, DiscardReason, Identity, LedgerUsage, QuotaPolicy};
use nexus_pricing::TokenUsage;
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// L'identita' che le FK del ledger esigono, dal seeder unico dello schema META,
/// piu' il `run_id`: quello non ha piu' una FK (mig 0276, i run agentici non
/// stanno in `orchestrator_runs`) e resta un UUID libero.
async fn seed_identita(pool: &PgPool) -> (Identity, Uuid) {
    let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(pool).await;
    (
        Identity {
            user_id,
            project_id,
        },
        Uuid::new_v4(),
    )
}

/// Listino con ENTRAMBE le tariffe di cache valorizzate (forma della mig 0403) e
/// le quattro tariffe DISTINTE: e' cio' che rende osservabile uno scambio.
async fn seed_listino_completo(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO ai_price_catalog ( \
             provider, model, \
             input_cost_per_million_tokens, output_cost_per_million_tokens, \
             cache_read_cost_per_million_tokens, cache_creation_cost_per_million_tokens, \
             currency, pricing_state \
         ) VALUES ('anthropic', 'claude-x', 3.0, 15.0, 0.3, 3.75, 'USD', 'priced')",
    )
    .execute(pool)
    .await
    .expect("seed ai_price_catalog");
}

/// Listino SENZA le tariffe di cache: e' la forma della maggioranza dei modelli
/// a catalog, non un caso di confine.
async fn seed_listino_senza_cache(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO ai_price_catalog ( \
             provider, model, \
             input_cost_per_million_tokens, output_cost_per_million_tokens, \
             currency, pricing_state \
         ) VALUES ('anthropic', 'claude-x', 3.0, 15.0, 'USD', 'priced')",
    )
    .execute(pool)
    .await
    .expect("seed listino senza tariffe di cache");
}

/// I token di una chiamata con cache: il prompt e' il LORDO, i 2M letti da cache
/// e i 0.5M scritti ne fanno parte, quindi 1M resta a tariffa piena.
fn tokens_con_cache() -> TokenUsage {
    TokenUsage {
        prompt_tokens: 3_500_000,
        completion_tokens: 400_000,
        cache_read_tokens: 2_000_000,
        cache_creation_tokens: 500_000,
    }
}

/// Prenota e finalizza per la strada della produzione, e ritorna il costo
/// REGISTRATO piu' l'id della riga.
async fn prenota_e_finalizza(pool: &PgPool, identity: Identity, run: Uuid) -> (Uuid, f64, String) {
    let tokens = tokens_con_cache();
    let reservation = nexus_ledger::reserve(
        pool,
        identity,
        "anthropic",
        "claude-x",
        tokens.prompt_tokens as i32,
        tokens.completion_tokens as i32,
        json!({ "feature": "test" }),
    )
    .await
    .expect("prenotazione");

    let settlement = nexus_ledger::finalize(pool, &reservation, run, &LedgerUsage::derived(tokens))
        .await
        .expect("finalizzazione");
    assert_eq!(
        settlement.charged_by,
        ChargedBy::Reservation,
        "senza una riga dichiarata da chi esegue, ad addebitare e' la prenotazione"
    );
    (
        reservation.ledger_id,
        settlement.total_cost,
        settlement.currency,
    )
}

/// I dodici numeri della riga, ognuno nella sua colonna.
///
/// Scelti DISTINTI a due a due proprio perche' uno scambio non possa passare
/// inosservato: 3.0, 6.0, 0.6, 1.875, 11.475 per gli importi; 3.5M, 400k, 2M,
/// 500k, 3.9M per i conteggi.
///
/// MUTAZIONE: scambiando fra loro due `.bind()` adiacenti nella UPDATE (per
/// esempio `cache_read_cost` e `cache_creation_cost`) il test fallisce dicendo
/// quale colonna ha preso il posto di quale; il compilatore non vede nulla,
/// perche' sono tutti `f64`.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn la_finalizzazione_scrive_ogni_numero_nella_sua_colonna(pool: PgPool) {
    let (identity, run) = seed_identita(&pool).await;
    seed_listino_completo(&pool).await;

    let (ledger_id, total_cost_dichiarato, currency) =
        prenota_e_finalizza(&pool, identity, run).await;
    assert_eq!(currency, "USD");

    let riga = sqlx::query(
        "SELECT run_id, status, details, \
                prompt_tokens, completion_tokens, total_tokens, \
                cache_read_tokens, cache_creation_tokens, \
                input_cost::float8          AS input_cost, \
                output_cost::float8         AS output_cost, \
                cache_read_cost::float8     AS cache_read_cost, \
                cache_creation_cost::float8 AS cache_creation_cost, \
                total_cost::float8          AS total_cost \
           FROM ai_usage_ledger WHERE id = $1",
    )
    .bind(ledger_id)
    .fetch_one(&pool)
    .await
    .expect("riga di ledger finalizzata");

    assert_eq!(riga.get::<String, _>("status"), "finalized");
    assert_eq!(riga.get::<Option<Uuid>, _>("run_id"), Some(run));

    // I quattro CONTEGGI, ognuno nella sua colonna. `prompt_tokens` e' il LORDO;
    // il totale e' lordo + completion (la cache e' gia' dentro il prompt).
    assert_eq!(riga.get::<i32, _>("prompt_tokens"), 3_500_000);
    assert_eq!(riga.get::<i32, _>("completion_tokens"), 400_000);
    assert_eq!(riga.get::<i64, _>("cache_read_tokens"), 2_000_000);
    assert_eq!(riga.get::<i64, _>("cache_creation_tokens"), 500_000);
    assert_eq!(riga.get::<i32, _>("total_tokens"), 3_900_000);

    // I cinque IMPORTI, alle quattro tariffe distinte del listino: 1M a tariffa
    // piena (3.5M lordi meno 2.5M di cache) x 3.0, 0.4M x 15.0, 2M x 0.3,
    // 0.5M x 3.75. Il messaggio riporta il valore LETTO: su uno scambio di bind
    // e' quello che dice quale colonna ha preso il posto di quale.
    let vicino = |a: f64, b: f64| (a - b).abs() < 1e-9;
    for (colonna, atteso) in [
        ("input_cost", 3.0),
        ("output_cost", 6.0),
        ("cache_read_cost", 0.6),
        ("cache_creation_cost", 1.875),
        ("total_cost", 11.475),
    ] {
        let letto: f64 = riga.get(colonna);
        assert!(
            vicino(letto, atteso),
            "{colonna}: letto {letto}, atteso {atteso}"
        );
    }

    // Cio' che la funzione DICHIARA al chiamante e cio' che ha SCRITTO sono la
    // stessa cosa: il costo annunciato al resto del sistema (metadata del
    // messaggio, budget del run) non puo' divergere dal ledger.
    assert!(vicino(total_cost_dichiarato, riga.get::<f64, _>("total_cost")));

    // Il segnale strutturato sullo stato del listino di CACHE, lo stesso che
    // mette chi scrive la riga dall'altro lato: qui le due tariffe sono a
    // listino, quindi 'priced'.
    let details: Value = riga.get("details");
    assert_eq!(details["cache_price_state"], "priced");
    // E la fusione con `||` non ha buttato via cio' che la prenotazione aveva
    // scritto: un assegnamento secco di `details` lo perderebbe.
    assert_eq!(details["feature"], "test");
}

/// Il caso che rende il segnale necessario: listino SENZA le tariffe di cache.
///
/// I due importi di cache finiscono a zero — identici a quelli di una chiamata
/// che la cache non l'ha usata — e solo `details` distingue i due casi. Senza
/// questa scrittura meta' delle righe del ledger resterebbe ambigua (regola M).
///
/// MUTAZIONE: facendo scrivere `"priced"` incondizionatamente in
/// `cache_price_state`, il test fallisce sulla prima asserzione.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn la_finalizzazione_dichiara_il_ripiego_a_tariffa_piena(pool: PgPool) {
    let (identity, run) = seed_identita(&pool).await;
    seed_listino_senza_cache(&pool).await;

    let (ledger_id, _, _) = prenota_e_finalizza(&pool, identity, run).await;

    let riga = sqlx::query(
        "SELECT details, cache_read_cost::float8 AS cache_read_cost, \
                input_cost::float8 AS input_cost \
           FROM ai_usage_ledger WHERE id = $1",
    )
    .bind(ledger_id)
    .fetch_one(&pool)
    .await
    .expect("riga di ledger finalizzata");

    let details: Value = riga.get("details");
    assert_eq!(
        details["cache_price_state"], "cache_price_missing",
        "senza tariffa di cache i token tornano a prezzo pieno: va DICHIARATO"
    );
    // L'importo di cache e' zero, e da solo non direbbe perche'.
    assert!(riga.get::<f64, _>("cache_read_cost").abs() < 1e-9);
    // I 2.5M di cache sono rientrati nel monte a tariffa piena: 3.5M x 3.0.
    assert!((riga.get::<f64, _>("input_cost") - 10.5).abs() < 1e-9);
}

/// La vista analitica legge il ledger con la premessa VERA (mig 0644).
///
/// Il test non interroga una tabella fabbricata: scrive la riga per la strada
/// della produzione (`reserve` -> `finalize`) e poi chiede alla VISTA, che e'
/// l'oggetto della migrazione. Cosi' la premessa del SQL e la convenzione del
/// codice non possono divergere in silenzio: se un domani il ledger tornasse al
/// prompt netto, questi numeri cambierebbero (regola O).
///
/// MUTAZIONE: rimettendo la formula della 0405
/// (`input_tokens_gross = prompt_tokens + cache_read_tokens`) il lordo diventa
/// 5.5M e l'hit-rate 0,3636.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn la_vista_analitica_non_doppia_i_token_di_cache(pool: PgPool) {
    let (identity, run) = seed_identita(&pool).await;
    seed_listino_completo(&pool).await;
    prenota_e_finalizza(&pool, identity, run).await;

    let riga = sqlx::query(
        "SELECT input_tokens_gross, prompt_tokens_net, total_tokens, \
                cache_hit_rate::float8 AS cache_hit_rate \
           FROM ai_usage_analytics_view \
          WHERE provider = 'anthropic' AND model = 'claude-x'",
    )
    .fetch_one(&pool)
    .await
    .expect("la vista deve esistere e vedere la riga finalizzata");

    // 3.5M lordi: i 2M letti da cache sono gia' dentro, sommarli darebbe 5.5M.
    assert_eq!(
        riga.get::<i64, _>("input_tokens_gross"),
        3_500_000,
        "l'input lordo E' prompt_tokens: la vecchia formula ci ri-sommava i cache_read"
    );
    // A tariffa piena resta 3.5M - 2M - 0.5M.
    assert_eq!(riga.get::<i64, _>("prompt_tokens_net"), 1_000_000);
    assert_eq!(riga.get::<i64, _>("total_tokens"), 3_900_000);
    // Hit-rate vero: 2M su 3.5M di contesto. Col denominatore gonfiato della
    // 0405 (5.5M) sarebbe uscito 0,3636.
    let hit: f64 = riga.get("cache_hit_rate");
    assert!(
        (hit - 0.5714).abs() < 1e-4,
        "hit-rate letto {hit}, atteso ~0.5714 (2M / 3.5M)"
    );
}

/// La spesa SCARTATA (mig 0701/0702): scritta dal produttore reale
/// (`record_discarded`), visibile nella vista, e con la quota che conta SOLO
/// le cause con usage osservato.
///
/// Tre affermazioni in un solo scenario, tutte per la strada della produzione:
///   1. la degenere porta token e costo REALI e nasce chiusa (`finalized_at`);
///   2. il timeout resta a zero con la causa dichiarata, non col silenzio;
///   3. la vista separa la spesa scartata dagli aggregati `finalized`
///      (una riga finalizzata accanto fa da controprova), e la quota vede la
///      degenere ma non il timeout.
///
/// MUTAZIONE: togliendo il ramo `discarded/degenerate_hollow` dal filtro di
/// `usage_for_quotas`, l'asserzione sulla quota scende a 3.9M; facendo nascere
/// la riga aperta (senza `finalized_at`), rosseggia l'asserzione dedicata.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn la_spesa_scartata_e_visibile_e_conta_in_quota_solo_se_osservata(pool: PgPool) {
    let (identity, run) = seed_identita(&pool).await;
    seed_listino_completo(&pool).await;

    // Una chiamata riuscita, per la controprova che gli aggregati finalized
    // non si mescolano con gli scarti.
    prenota_e_finalizza(&pool, identity, run).await;

    // La risposta DEGENERE: usage reale dal wire (1M prompt di cui 0.9M da
    // cache, 0 completion — la forma tipica del caso misurato).
    let usage_degenere = TokenUsage {
        prompt_tokens: 1_000_000,
        completion_tokens: 0,
        cache_read_tokens: 900_000,
        cache_creation_tokens: 0,
    };
    nexus_ledger::record_discarded(
        &pool,
        Some(identity),
        "anthropic",
        "claude-x",
        DiscardReason::DegenerateHollow,
        Some(&usage_degenere),
        &run.to_string(),
        "chat",
    )
    .await;

    // Il cap per-tentativo scaduto: nessun usage osservato.
    nexus_ledger::record_discarded(
        &pool,
        Some(identity),
        "anthropic",
        "claude-x",
        DiscardReason::AttemptTimeout,
        None,
        &run.to_string(),
        "chat",
    )
    .await;

    // 1+2: le due righe, ognuna con la sua causa e i suoi numeri.
    let righe = sqlx::query(
        "SELECT discard_reason, total_tokens, total_cost::float8 AS total_cost, \
                finalized_at IS NOT NULL AS chiusa \
           FROM ai_usage_ledger WHERE status = 'discarded' ORDER BY discard_reason",
    )
    .fetch_all(&pool)
    .await
    .expect("le righe discarded devono esistere: la INSERT e' best-effort e un \
             errore sarebbe solo loggato");
    assert_eq!(righe.len(), 2);
    let timeout = &righe[0]; // attempt_timeout < degenerate_hollow
    assert_eq!(timeout.get::<String, _>("discard_reason"), "attempt_timeout");
    assert_eq!(timeout.get::<i32, _>("total_tokens"), 0);
    assert!(timeout.get::<f64, _>("total_cost").abs() < 1e-12);
    let degenere = &righe[1];
    assert_eq!(
        degenere.get::<String, _>("discard_reason"),
        "degenerate_hollow"
    );
    assert_eq!(degenere.get::<i32, _>("total_tokens"), 1_000_000);
    // 100k a 3.0 + 900k a 0.3 = 0.57: il costo della degenere e' scorporato
    // dalla cache come ogni riga vera.
    assert!((degenere.get::<f64, _>("total_cost") - 0.57).abs() < 1e-9);
    for r in &righe {
        assert!(
            r.get::<bool, _>("chiusa"),
            "una riga discarded nasce CHIUSA: aperta sarebbe prenotabile"
        );
    }

    // 3a: la vista separa scarti e finalized.
    let vista = sqlx::query(
        "SELECT calls, total_tokens, discarded_calls, discarded_tokens, \
                discarded_cost::float8 AS discarded_cost \
           FROM ai_usage_analytics_view \
          WHERE provider = 'anthropic' AND model = 'claude-x'",
    )
    .fetch_one(&pool)
    .await
    .expect("la vista deve vedere il bucket");
    assert_eq!(vista.get::<i64, _>("calls"), 1, "solo la finalizzata");
    assert_eq!(
        vista.get::<i64, _>("total_tokens"),
        3_900_000,
        "gli aggregati finalized non assorbono gli scarti"
    );
    assert_eq!(vista.get::<i64, _>("discarded_calls"), 2);
    assert_eq!(
        vista.get::<i64, _>("discarded_tokens"),
        1_000_000,
        "la degenere porta i suoi token, il timeout zero"
    );
    assert!((vista.get::<f64, _>("discarded_cost") - 0.57).abs() < 1e-9);

    // 3b: la quota conta la degenere (spesa reale) e NON il timeout.
    let quota = QuotaPolicy {
        scope_type: "project".into(),
        user_id: None,
        project_id: Some(identity.project_id),
        token_limit: Some(i64::MAX),
        cost_limit: None,
        valid_from: chrono::Utc::now() - chrono::Duration::hours(1),
        valid_to: chrono::Utc::now() + chrono::Duration::hours(1),
    };
    let consumi = nexus_ledger::usage_for_quotas(&pool, &[quota])
        .await
        .expect("lettura consumo");
    assert_eq!(
        consumi[0].tokens,
        3_900_000 + 1_000_000,
        "finalizzata + degenere; il timeout (zero osservato) non entra"
    );
}

/// Lo STESSO scenario del ripiego, ma letto dalla vista: e' il caso in cui
/// `prompt_tokens_net` non puo' essere "lordo meno la cache".
///
/// Quando il listino non ha la tariffa di cache, `calculate_cost_breakdown`
/// rimette quei token nel monte a tariffa piena invece di regalarli. Se la vista
/// li sottraesse comunque, la colonna smetterebbe di essere divisibile per il
/// costo scritto accanto: chi fa `input_cost / prompt_tokens_net` leggerebbe
/// 10,5 / 1M = 10.5 $/M invece dei 3.0 $/M di catalog, e concluderebbe che il
/// calcolo del costo e' rotto — mentre a mentire sarebbe la colonna.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn la_vista_non_scorpora_la_cache_fatturata_a_tariffa_piena(pool: PgPool) {
    let (identity, run) = seed_identita(&pool).await;
    seed_listino_senza_cache(&pool).await;
    prenota_e_finalizza(&pool, identity, run).await;

    let riga = sqlx::query(
        "SELECT v.prompt_tokens_net, v.input_tokens_gross, \
                l.input_cost::float8 AS input_cost \
           FROM ai_usage_analytics_view v \
           JOIN ai_usage_ledger l ON l.provider = v.provider AND l.model = v.model \
          WHERE v.provider = 'anthropic' AND v.model = 'claude-x'",
    )
    .fetch_one(&pool)
    .await
    .expect("la vista deve vedere la riga finalizzata");

    let a_tariffa_piena = riga.get::<i64, _>("prompt_tokens_net");
    assert_eq!(
        a_tariffa_piena, 3_500_000,
        "senza tariffa di cache il monte a tariffa piena e' il LORDO intero: \
         sottrarre i 2,5M di cache darebbe 1.000.000, cioe' token mai fatturati"
    );
    assert_eq!(riga.get::<i64, _>("input_tokens_gross"), 3_500_000);

    // Il punto della colonna: resta divisibile per il costo scritto accanto.
    let tariffa_implicita =
        riga.get::<f64, _>("input_cost") / (a_tariffa_piena as f64 / 1_000_000.0);
    assert!(
        (tariffa_implicita - 3.0).abs() < 1e-9,
        "tariffa implicita {tariffa_implicita} $/M, attesa 3.0 come a catalog"
    );
}
