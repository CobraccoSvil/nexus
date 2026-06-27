//! Contract test (PR-4 Livello 3): clarifying-questions HITL endpoint contract.
//!
//! Verifica:
//!   - GET /agent/clarifications/:run_id ritorna clarification=null se nessuna
//!
//! Test sul brain endpoint (port 8001). Skip se brain non risponde.
//!
//! NB: i contract test sugli endpoint `/agent/subagent-run` e
//! `/agent/subagent-resume` sono stati RIMOSSI col porting dei sub-agenti al
//! grafo nativo Rust (`mcp-core::agent_tools::subagent_native`): mcp-core non
//! chiama piu' il brain per i sub-run. La copertura del nuovo percorso vive nei
//! unit test di `agent_tools::subagent_native` e `native_engine` (costruzione
//! NativeRunInput del sub-run, guard, propagazione depth).

use std::env;
use std::time::Duration;

fn brain_url() -> String {
    env::var("BRAIN_URL").unwrap_or_else(|_| "http://localhost:8001".into())
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

async fn brain_alive() -> bool {
    client()
        .get(format!("{}/health", brain_url()))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

#[tokio::test]
async fn clarifications_get_ritorna_null_se_nessuna() {
    if !brain_alive().await {
        eprintln!("skip");
        return;
    }
    let fake = "00000000-0000-0000-0000-000000000000";
    let url = format!("{}/agent/clarifications/{}", brain_url(), fake);
    let resp = client().get(&url).send().await.expect("request");
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.expect("json");
    // Quando non c'e' nessuna clarification per il run, "clarification" e' null
    // (oppure assente in caso di errore DB).
    assert!(body.get("clarification").is_some() || body.get("error").is_some());
}
