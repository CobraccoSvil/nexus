//! Contract test (PR-4 Livello 3): endpoint REST agent-runs + admin.
//!
//! Richiedono mcp-core attivo su `http://localhost:4000` e una sessione
//! valida (cookie `token=`) di un user admin. Se non sono entrambi presenti
//! il test viene skippato con `eprintln!` per non rompere CI dev box.
//!
//! Setup tipico:
//!   1. `cargo build -p mcp-core --release && ./target/release/mcp-core &`
//!   2. `bash /tmp/mint_jwt.sh`  (helper JWT admin per i test protetti)
//!   3. `MCP_CORE_URL=http://localhost:4000 NEXUS_TEST_JWT=$(cat /tmp/nexus_jwt.txt) cargo test --test agent_runs_endpoints`

use std::time::Duration;
use nexus_test_preconditions::{base_url, jwt_o_salta, salta, Motivo};

/// Client HTTP per i contract test, che richiedono auth admin: se il JWT manca il
/// punto unico dichiara lo skip (o fallisce sotto REQUIRE_INTEGRATION_TESTS=1).
async fn client() -> Option<reqwest::Client> {
    jwt_o_salta()?;
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .ok()
}

/// Il token viaggia nel COOKIE: `nexus_auth::validate_token` lo estrae solo da li'.
/// Chiamata solo dopo `client()`, che ha gia' verificato la presenza del JWT.
async fn cookie_header() -> String {
    format!("token={}", std::env::var("NEXUS_TEST_JWT").unwrap_or_default())
}

#[tokio::test]
async fn health_endpoint_e_raggiungibile() {
    let url = format!("{}/health", base_url());
    let resp = reqwest::get(&url).await;
    if resp.is_err() {
        salta(Motivo::ServizioGiu(&url));
        return;
    }
    let r = resp.unwrap();
    assert!(
        r.status().is_success(),
        "health endpoint deve rispondere 2xx"
    );
}

#[tokio::test]
async fn admin_settings_endpoint_richiede_auth() {
    let Some(client) = client().await else { return };
    let url = format!("{}/api/admin/settings", base_url());
    // Senza cookie → 401
    let r = client.get(&url).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 401, "senza cookie deve essere 401");
    // Con cookie admin → 200
    let r = client
        .get(&url)
        .header("Cookie", cookie_header().await)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200, "con cookie admin deve essere 200");
}

#[tokio::test]
async fn reset_cooldown_endpoint_idempotente() {
    let Some(client) = client().await else { return };
    let provider = "anthropic";
    let url = format!(
        "{}/api/admin/providers/{}/reset-cooldown",
        base_url(),
        provider
    );
    // Prima chiamata: rimuove (ok=true)
    let r1 = client
        .post(&url)
        .header("Cookie", cookie_header().await)
        .send()
        .await
        .unwrap();
    assert_eq!(r1.status().as_u16(), 200);
    let body: serde_json::Value = r1.json().await.unwrap();
    assert_eq!(body["ok"], serde_json::json!(true));
    assert_eq!(body["provider"], serde_json::json!(provider));
    // Seconda chiamata (idempotente): ancora ok=true
    let r2 = client
        .post(&url)
        .header("Cookie", cookie_header().await)
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status().as_u16(), 200);
}

// Il contract test del bridge `POST /api/internal/provider-error` e' caduto col
// bridge (13/08/2026): era un terzo scrittore di esclusioni che classificava
// dalla prosa, e il suo unico client Python non esiste piu'. L'esclusione che
// conta oggi la dichiara il gateway a ogni chiamata, ed e' provata in-process
// dove la produzione la attraversa (`mcp_core::nexus_gateway` -> `complete`).
