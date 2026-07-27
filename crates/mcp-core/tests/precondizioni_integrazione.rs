//! SENTINELLA delle precondizioni di integrazione di mcp-core: gira SEMPRE e
//! dichiara da dove guardano gli altri test di questo crate.
//!
//! Regola O, "un numero senza la sua premessa e' un'opinione". I test di
//! integrazione sono verdi in due casi opposti — hanno verificato il contratto,
//! oppure non hanno trovato l'ambiente e sono usciti subito — e nel conteggio di
//! `cargo test` i due casi sono identici. Questo test rende la premessa esplicita
//! nell'output: quali precondizioni ci sono, quali no, e quindi quali gruppi di
//! test hanno misurato qualcosa.
//!
//! La stampa e l'asserzione vengono dal punto unico
//! (`nexus_test_preconditions::dichiara_quadro` / `pretendi_ambiente_completo`):
//! qui vive solo l'elenco delle precondizioni di QUESTO crate, l'unica cosa
//! specifica. Senza `REQUIRE_INTEGRATION_TESTS=1` un ambiente incompleto e'
//! legittimo (in CI mancano JWT e servizio in ascolto, per scelta); con quella
//! variabile l'ambiente dichiara di essere completo e ogni pezzo assente e' un
//! fallimento.

use nexus_test_preconditions::{
    base_url, db_risponde, dichiara_quadro, env_presente, pretendi_ambiente_completo, Stato,
};

/// Il servizio risponde? Si interroga l'endpoint di salute con un timeout corto:
/// senza timeout, un host che accetta la connessione e non risponde bloccherebbe
/// la sentinella.
async fn mcp_core_risponde() -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    else {
        return false;
    };
    client
        .get(format!("{}/health", base_url()))
        .send()
        .await
        .is_ok()
}

#[tokio::test]
async fn precondizioni_dichiarate() {
    let stati = [
        Stato {
            nome: "DATABASE_URL",
            presente: env_presente("DATABASE_URL"),
            conseguenza:
                "contract test su schema e dati (orchestrator_db_schema, project_db_config_contract)",
        },
        Stato {
            nome: "DB raggiungibile",
            presente: db_risponde().await,
            conseguenza: "tutto cio' che legge o semina righe",
        },
        Stato {
            nome: "NEXUS_TEST_JWT",
            presente: env_presente("NEXUS_TEST_JWT"),
            conseguenza:
                "ogni richiesta autenticata (settings_update_contract, m71_cost_breakdown, agent_runs_endpoints)",
        },
        Stato {
            nome: "mcp-core in ascolto",
            presente: mcp_core_risponde().await,
            conseguenza: "ogni contratto verificato al wire HTTP",
        },
    ];

    let mancanti = dichiara_quadro(&format!("mcp-core (url: {})", base_url()), &stati);
    pretendi_ambiente_completo(&mancanti, stati.len());
}
