//! SENTINELLA delle precondizioni di integrazione: gira SEMPRE e dichiara da
//! dove guardano gli altri test di questo crate.
//!
//! Regola O, "un numero senza la sua premessa e' un'opinione". I test di
//! integrazione di mcp-core sono verdi in due casi opposti — hanno verificato il
//! contratto, oppure non hanno trovato l'ambiente e sono usciti subito — e nel
//! conteggio di `cargo test` i due casi sono identici. Questo test rende la
//! premessa esplicita nell'output: quali precondizioni ci sono, quali no, e
//! quindi quali gruppi di test hanno misurato qualcosa.
//!
//! Non asserisce nulla sull'ambiente: senza `REQUIRE_INTEGRATION_TESTS=1` un
//! ambiente incompleto e' legittimo (in CI mancano JWT e servizio in ascolto, per
//! scelta: vedi `tests/support/mod.rs`). Con quella variabile, invece, l'ambiente
//! dichiara di essere completo e ogni pezzo assente e' un fallimento.

mod support;

use support::{base_url, richiede_integrazione, REQUIRE_ENV};

/// Nome della precondizione e sua disponibilita' effettiva.
struct Stato {
    nome: &'static str,
    presente: bool,
    /// Cosa resta non misurato quando manca.
    conseguenza: &'static str,
}

/// Il servizio risponde? Si interroga l'endpoint di salute con un timeout corto:
/// senza timeout, un host che accetta la connessione e non risponde bloccherebbe
/// la sentinella.
async fn mcp_core_risponde() -> bool {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    client
        .get(format!("{}/health", base_url()))
        .send()
        .await
        .is_ok()
}

/// Il DB di `DATABASE_URL` accetta connessioni? Distingue "variabile assente" da
/// "DB che non risponde": sono due guasti diversi con due rimedi diversi.
async fn db_risponde() -> bool {
    let Some(url) = std::env::var("DATABASE_URL").ok().filter(|s| !s.is_empty()) else {
        return false;
    };
    sqlx::PgPool::connect(&url).await.is_ok()
}

#[tokio::test]
async fn precondizioni_dichiarate() {
    let jwt = std::env::var("NEXUS_TEST_JWT")
        .ok()
        .is_some_and(|s| !s.is_empty());
    let db_var = std::env::var("DATABASE_URL")
        .ok()
        .is_some_and(|s| !s.is_empty());

    let stati = vec![
        Stato {
            nome: "DATABASE_URL",
            presente: db_var,
            conseguenza: "contract test su schema e dati (orchestrator_db_schema, project_db_config_contract)",
        },
        Stato {
            nome: "DB raggiungibile",
            presente: db_risponde().await,
            conseguenza: "tutto cio' che legge o semina righe",
        },
        Stato {
            nome: "NEXUS_TEST_JWT",
            presente: jwt,
            conseguenza: "ogni richiesta autenticata (settings_update_contract, m71_cost_breakdown, agent_runs_endpoints)",
        },
        Stato {
            nome: "mcp-core in ascolto",
            presente: mcp_core_risponde().await,
            conseguenza: "ogni contratto verificato al wire HTTP",
        },
    ];

    let presenti = stati.iter().filter(|s| s.presente).count();
    println!(
        "PRECONDIZIONI INTEGRAZIONE mcp-core: {presenti}/{} presenti (url: {})",
        stati.len(),
        base_url()
    );
    for s in &stati {
        let segno = if s.presente { "OK  " } else { "MANCA" };
        println!("  {segno} {} -> {}", s.nome, s.conseguenza);
    }

    let mancanti: Vec<&str> = stati
        .iter()
        .filter(|s| !s.presente)
        .map(|s| s.nome)
        .collect();
    if !mancanti.is_empty() {
        println!(
            "  I test che dipendono da [{}] saltano: verdi senza aver misurato nulla.",
            mancanti.join(", ")
        );
    }

    let ambiente_incompleto = richiede_integrazione() && !mancanti.is_empty();
    assert!(
        !ambiente_incompleto,
        "{REQUIRE_ENV}=1 ma mancano {} precondizioni su {}: {}. \
         I contratti che ne dipendono NON sono verificati.",
        mancanti.len(),
        stati.len(),
        mancanti.join(", ")
    );
}
