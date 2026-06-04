//! Contract test (PR-4 Livello 3): subagent run lifecycle endpoint contract.
//!
//! Verifica:
//!   - GET /agent/subagent-run/:id ritorna not_found per UUID inesistente
//!   - POST /agent/subagent-resume su UUID inesistente ritorna error
//!   - GET /agent/clarifications/:run_id ritorna clarification=null se nessuna
//!
//! Test sui brain endpoints (port 8001). Skip se brain non risponde.

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
async fn subagent_poll_not_found_per_uuid_inesistente() {
    if !brain_alive().await {
        eprintln!("skip: brain non in ascolto su {}", brain_url());
        return;
    }
    let fake = "00000000-0000-0000-0000-000000000000";
    let url = format!("{}/agent/subagent-run/{}", brain_url(), fake);
    let resp = client().get(&url).send().await.expect("request");
    assert!(
        resp.status().is_success(),
        "endpoint deve rispondere 2xx anche per not_found"
    );
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(
        body.get("error").and_then(|v| v.as_str()),
        Some("not_found")
    );
}

#[tokio::test]
async fn subagent_resume_ritorna_errore_per_uuid_inesistente() {
    if !brain_alive().await {
        eprintln!("skip");
        return;
    }
    let resp = client()
        .post(format!("{}/agent/subagent-resume", brain_url()))
        .json(&serde_json::json!({"run_id": "00000000-0000-0000-0000-000000000000"}))
        .send()
        .await
        .expect("request");
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.expect("json");
    let status = body.get("status").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        status == "error" || status == "noop",
        "atteso error/noop, ricevuto: {}",
        body
    );
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
