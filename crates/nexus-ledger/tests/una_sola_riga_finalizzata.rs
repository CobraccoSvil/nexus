//! Una chiamata, UNA riga finalizzata.
//!
//! Questo file e' il motivo per cui il crate esiste. La verifica percorre
//! ENTRAMBI i produttori reali — chi prenota prima della chiamata e chi scrive
//! dopo averla eseguita — sullo stesso database, e nessuno dei due e' fabbricato.
//!
//! Prima non era scrivibile. I due scrittori vivevano in due crate che non si
//! vedevano (`mcp-core` e `nexus-gateway`), quindi la verifica era spezzata in
//! due test in due posti, e quello di mcp-core doveva SEMINARE a mano la riga
//! del gateway con una INSERT ricopiata, dichiarando la premessa in un commento:
//! "il suo produttore vero vive in un crate che mcp-core non vede". Un test che
//! fabbrica un input gia' prodotto altrove fissa l'assunto che dovrebbe
//! verificare, e resta verde anche quando i due produttori divergono (regola O).
//!
//! Il difetto che qui si tiene chiuso e' del 2026-07-27: un solo messaggio in
//! chat, due righe `finalized` con lo stesso `run_id`, stessi token, stesso
//! costo. 0.002339 addebitati due volte.

use nexus_ledger::{ChargedBy, Declaration, Identity, LedgerOutcome, LedgerUsage, QuotaLock};
use nexus_pricing::TokenUsage;
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Listino con le quattro tariffe distinte (forma della mig 0403).
async fn seed_listino(pool: &PgPool) {
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

/// L'identita' che le FK del ledger esigono, dal seeder unico dello schema META.
async fn identita(pool: &PgPool) -> Identity {
    let (user_id, project_id) = nexus_migrations_embedded::seed_identita_meta(pool).await;
    Identity {
        user_id,
        project_id,
    }
}

/// I token di una chiamata da 1M di prompt e 400k di completion, senza cache.
fn tokens_della_chiamata() -> TokenUsage {
    TokenUsage::senza_cache(1_000_000, 400_000)
}

/// Le righe `finalized` di un run, come le vede chi legge il ledger.
async fn righe_finalizzate(pool: &PgPool, run: Uuid) -> Vec<(Uuid, f64)> {
    sqlx::query_as(
        "SELECT id, total_cost::float8 FROM ai_usage_ledger \
          WHERE run_id = $1 AND status = 'finalized'",
    )
    .bind(run)
    .fetch_all(pool)
    .await
    .expect("righe del run")
}

/// Il denaro dell'intero progetto, senza filtrare per stato.
///
/// Il filtro NON si mette apposta: `usage_report` (report admin/progetto/utente)
/// somma `total_cost` di ogni riga che passa i suoi filtri, e una prenotazione
/// rilasciata che conservasse la propria stima sarebbe lo stesso doppio addebito
/// con un vestito diverso.
async fn totale_addebitato(pool: &PgPool, identity: Identity) -> f64 {
    sqlx::query_scalar(
        "SELECT COALESCE(SUM(total_cost), 0)::float8 FROM ai_usage_ledger WHERE project_id = $1",
    )
    .bind(identity.project_id)
    .fetch_one(pool)
    .await
    .expect("somma del ledger")
}

/// IL test: una chiamata riuscita lascia una sola riga finalizzata.
///
/// Il percorso e' quello di produzione per intero e nell'ordine vero:
///   1. chi sta per chiamare PRENOTA (`reserve`), che e' anche il gate quote;
///   2. chi ESEGUE la chiamata scrive la propria riga e la DICHIARA
///      (`record_tokens` -> `LedgerEntry`);
///   3. chi aveva prenotato CHIUDE (`settle`), leggendo quella dichiarazione.
///
/// MUTAZIONE: sostituendo il corpo di `settle` con la finalizzazione
/// incondizionata (il comportamento pre-fix) il conteggio sale a 2 e la somma a
/// 18.0; togliendo l'azzeramento dalla UPDATE di rilascio il conteggio resta 1
/// ma la somma diventa 18.0 lo stesso, per via della stima rimasta sulla riga
/// rilasciata.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn una_chiamata_lascia_una_sola_riga_finalizzata(pool: PgPool) {
    let identity = identita(&pool).await;
    seed_listino(&pool).await;
    let run = Uuid::new_v4();
    let tokens = tokens_della_chiamata();

    // 1. La prenotazione, col contesto che solo chi orchestra conosce.
    let reservation = nexus_ledger::reserve(
        &pool,
        identity,
        "anthropic",
        "claude-x",
        1_000_000,
        400_000,
        json!({ "intent": "chat", "profile_id": "p-1", "corrections_count": 2 }),
    )
    .await
    .expect("prenotazione");

    // 2. Chi esegue scrive e dichiara. Il `request_id` e' il run: e' cosi' che la
    //    riga si aggancia al run in produzione.
    let entry = nexus_ledger::record_tokens(
        &pool,
        identity,
        "anthropic",
        "claude-x",
        &tokens,
        None,
        &run.to_string(),
        "chat",
    )
    .await
    .expect("chi esegue ha scritto: deve dichiarare la riga");

    // 3. Chi aveva prenotato chiude, leggendo la dichiarazione.
    let settlement = nexus_ledger::settle(
        &pool,
        &reservation,
        run,
        &LedgerUsage::derived(tokens),
        &Declaration::Detta(LedgerOutcome::Written(entry.clone())),
    )
    .await
    .expect("chiusura contabile");

    // UNA sola riga finalizzata, ed e' quella di chi ha eseguito: porta il
    // provider e il modello EFFETTIVI della chiamata, non la stima.
    let righe = righe_finalizzate(&pool, run).await;
    assert_eq!(
        righe.len(),
        1,
        "una chiamata deve lasciare UNA riga finalizzata, trovate {}: {righe:?}",
        righe.len()
    );
    assert_eq!(righe[0].0, entry.id);

    // E il denaro del progetto e' quello di una chiamata sola: 1M x 3.0 + 0.4M x
    // 15.0 = 9.0, non 18.0.
    let totale = totale_addebitato(&pool, identity).await;
    assert!(
        (totale - 9.0).abs() < 1e-9,
        "addebitato {totale} per UNA chiamata, atteso 9.0"
    );

    // La prenotazione non e' sparita: e' rilasciata, non conta piu' per le quote,
    // e conserva il contesto che chi esegue non ha, piu' il puntatore alla riga
    // che addebita davvero.
    let prenotazione = sqlx::query(
        "SELECT status, details, total_tokens, total_cost::float8 AS total_cost \
           FROM ai_usage_ledger WHERE id = $1",
    )
    .bind(reservation.ledger_id)
    .fetch_one(&pool)
    .await
    .expect("riga della prenotazione");
    assert_eq!(prenotazione.get::<String, _>("status"), "released");
    assert_eq!(prenotazione.get::<i32, _>("total_tokens"), 0);
    assert!(prenotazione.get::<f64, _>("total_cost").abs() < 1e-9);
    let details: Value = prenotazione.get("details");
    assert_eq!(details["intent"], "chat");
    assert_eq!(details["corrections_count"], 2);
    assert_eq!(
        details["superseded_by_ledger_id"],
        json!(entry.id.to_string()),
        "la prenotazione rilasciata deve dire QUALE riga porta l'addebito"
    );
    // La stima non e' distrutta: e' traslocata dove nessuno la somma.
    assert!(details["released_estimate"]["total_cost"]
        .as_f64()
        .expect("stima traslocata")
        > 0.0);

    // E cio' che il chiamante annuncia al resto del sistema (metadata del
    // messaggio, budget del run) e' il costo REGISTRATO, non la stima.
    assert_eq!(settlement.charged_by, ChargedBy::Executor);
    assert!((settlement.total_cost - 9.0).abs() < 1e-9);
    assert_eq!(settlement.currency, "USD");
}

/// L'altra faccia: senza una riga dichiarata, l'addebito non si perde. E vale
/// per TUTTI i modi in cui una riga puo' mancare, non solo per quello atteso.
///
/// `NoIdentity` e' il caso reale di `NeuralCoreClient::generate_completion`, che
/// manda metadata senza identita'. Ma `Muta` (chi esegue non parla questa
/// versione del contratto) e `Illeggibile` (dichiarazione presente e non
/// deserializzabile) arrivano allo stesso punto per strade diverse, ed e' li'
/// che il difetto si era gia' infilato una volta: un `.ok()` trasformava una
/// malformazione in "nessuno ha addebitato". Qualunque sia la strada, l'unica
/// chiusura che non perde l'addebito e' finalizzare.
///
/// MUTAZIONE: facendo rilasciare anche il ramo senza riga, le righe finalizzate
/// del run diventano zero per tutti e tre i casi.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn senza_riga_dichiarata_la_prenotazione_viene_finalizzata(pool: PgPool) {
    let identity = identita(&pool).await;
    seed_listino(&pool).await;

    for dichiarazione in [
        Declaration::Detta(LedgerOutcome::NoIdentity),
        Declaration::Detta(LedgerOutcome::WriteFailed),
        Declaration::Muta,
        Declaration::Illeggibile,
    ] {
        let run = Uuid::new_v4();
        let etichetta = dichiarazione.as_str();
        let reservation = nexus_ledger::reserve(
            &pool,
            identity,
            "anthropic",
            "claude-x",
            1_000_000,
            400_000,
            json!({ "feature": "batch" }),
        )
        .await
        .expect("prenotazione");

        let settlement = nexus_ledger::settle(
            &pool,
            &reservation,
            run,
            &LedgerUsage::derived(tokens_della_chiamata()),
            &dichiarazione,
        )
        .await
        .expect("chiusura contabile");

        let righe = righe_finalizzate(&pool, run).await;
        assert_eq!(
            righe.len(),
            1,
            "dichiarazione '{etichetta}': l'addebito non deve sparire, trovate {righe:?}"
        );
        assert_eq!(righe[0].0, reservation.ledger_id);
        // Il costo REALE, non uno zero di cortesia.
        assert!(
            (righe[0].1 - 9.0).abs() < 1e-9,
            "dichiarazione '{etichetta}': costo scritto {}",
            righe[0].1
        );
        assert_eq!(settlement.charged_by, ChargedBy::Reservation);
        assert!((settlement.total_cost - 9.0).abs() < 1e-9);
    }
}

/// Il vincolo da non rompere: la prenotazione e' il gate delle QUOTE.
///
/// Le tre fasi con la stessa quota: prenotazione che occupa (PRIMA della
/// chiamata), rilascio, consumo che continua a essere visto (DOPO, dalla riga di
/// chi ha eseguito). Le due asserzioni di rifiuto da sole non basterebbero —
/// passerebbero anche se il consumo fosse contato DUE volte — per questo in
/// mezzo c'e' una prenotazione che deve RIUSCIRE: e' quella a dimostrare che il
/// rilascio non lascia un consumo fantasma.
///
/// MUTAZIONE: con la doppia finalizzazione il consumo pesa 4000 token invece di
/// 2000 e la terza prenotazione viene respinta.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn la_quota_vede_il_consumo_prima_e_dopo_la_chiamata(pool: PgPool) {
    let identity = identita(&pool).await;
    seed_listino(&pool).await;
    let run = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO ai_quota_policies \
             (scope_type, project_id, token_limit, valid_from, valid_to) \
         VALUES ('project', $1, 3000, NOW() - INTERVAL '1 hour', NOW() + INTERVAL '1 hour')",
    )
    .bind(identity.project_id)
    .execute(&pool)
    .await
    .expect("seed quota");

    let prenota = |tokens: i32| {
        let pool = pool.clone();
        async move {
            nexus_ledger::reserve(
                &pool,
                identity,
                "anthropic",
                "claude-x",
                tokens,
                0,
                json!({ "feature": "quota" }),
            )
            .await
        }
    };

    // PRIMA della chiamata: la prenotazione occupa quota.
    let reservation = prenota(2000).await.expect("prima prenotazione");
    assert!(
        prenota(2000).await.is_err(),
        "2000 gia' prenotati + 2000 sfora il limite di 3000: la prenotazione DEVE \
         occupare quota anche prima che la chiamata sia partita"
    );

    // La chiamata: chi esegue scrive la sua riga, chi aveva prenotato rilascia.
    let tokens = TokenUsage::senza_cache(2000, 0);
    let entry = nexus_ledger::record_tokens(
        &pool,
        identity,
        "anthropic",
        "claude-x",
        &tokens,
        None,
        &run.to_string(),
        "quota",
    )
    .await
    .expect("riga di chi ha eseguito");
    nexus_ledger::settle(
        &pool,
        &reservation,
        run,
        &LedgerUsage::derived(tokens),
        &Declaration::Detta(LedgerOutcome::Written(entry)),
    )
    .await
    .expect("chiusura contabile");

    // DOPO: il consumo e' ancora visto, ma UNA volta sola. 2000 usati su 3000:
    // 1000 devono ancora passare.
    assert!(
        prenota(1000).await.is_ok(),
        "consumo contato due volte: dopo il rilascio il ledger deve pesare 2000 token \
         (la riga di chi ha eseguito), non 4000"
    );
    // E il limite resta un limite.
    assert!(
        prenota(1).await.is_err(),
        "3000/3000 esauriti: la quota deve continuare a respingere dopo la chiamata"
    );
}

/// Una quota di COSTO si legge e si applica.
///
/// Non e' un caso di confine: e' la divergenza che le due copie avevano gia'
/// addosso. `ai_quota_policies.cost_limit` e' `NUMERIC(18,6)` e sqlx non
/// decodifica un `NUMERIC` in `f64`; la query del gateway aveva il cast a
/// `float8`, quella di mcp-core no. Finche' nessuno ha configurato una quota di
/// costo, mcp-core non ha mai avuto una riga da decodificare e l'errore non e'
/// mai arrivato — sarebbe arrivato al primo cliente con un tetto di spesa, come
/// fallimento di `reserve`, cioe' come chat che non risponde piu'.
///
/// MUTAZIONE: togliendo `::float8` dalla SELECT delle quote, questo test
/// fallisce con "mismatched types; Rust type `f64` is not compatible with SQL
/// type `NUMERIC`", mentre tutti gli altri restano verdi.
#[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
async fn una_quota_di_costo_si_legge_e_si_applica(pool: PgPool) {
    let identity = identita(&pool).await;
    seed_listino(&pool).await;
    sqlx::query(
        "INSERT INTO ai_quota_policies \
             (scope_type, project_id, cost_limit, valid_from, valid_to) \
         VALUES ('project', $1, 10.0, NOW() - INTERVAL '1 hour', NOW() + INTERVAL '1 hour')",
    )
    .bind(identity.project_id)
    .execute(&pool)
    .await
    .expect("seed quota di costo");

    // La riga si legge: prima ancora di applicarla, decodificarla non deve
    // fallire.
    let quotas = nexus_ledger::active_quotas(
        &pool,
        identity.user_id,
        identity.project_id,
        QuotaLock::None,
    )
    .await
    .expect("le quote di costo devono essere leggibili");
    assert_eq!(quotas.len(), 1);
    assert_eq!(quotas[0].cost_limit, Some(10.0));

    // E si applica: 1M x 3.0 + 0.4M x 15.0 = 9.0 ci sta sotto i 10.0...
    let reservation = nexus_ledger::reserve(
        &pool,
        identity,
        "anthropic",
        "claude-x",
        1_000_000,
        400_000,
        json!({ "feature": "quota-costo" }),
    )
    .await
    .expect("9.0 su un tetto di 10.0 deve passare");
    assert!(!reservation.ledger_id.is_nil());

    // ...ma la seconda no, perche' la prima occupa gia' 9.0.
    let sforata = nexus_ledger::reserve(
        &pool,
        identity,
        "anthropic",
        "claude-x",
        1_000_000,
        400_000,
        json!({ "feature": "quota-costo" }),
    )
    .await;
    let e = sforata.expect_err("9.0 + 9.0 su un tetto di 10.0 deve sforare");
    assert_eq!(e.to_string(), "quota_exceeded:project:cost_limit");

    // E il rifiuto e' un FATTO contabile: la riga esiste, con il suo motivo.
    let rifiutata: (String, String) = sqlx::query_as(
        "SELECT status, rejection_reason FROM ai_usage_ledger \
          WHERE project_id = $1 AND status = 'rejected'",
    )
    .bind(identity.project_id)
    .fetch_one(&pool)
    .await
    .expect("la riga di rifiuto deve essere stata scritta");
    assert_eq!(rifiutata.0, "rejected");
    assert_eq!(rifiutata.1, "quota_exceeded:project:cost_limit");
}
