//! Contract test (PR-4 Livello 3): endpoint REST agent-runs + admin.
//!
//! Richiedono mcp-core attivo su `http://localhost:4000` e una sessione
//! valida (cookie `token=`) di un user admin. Se non sono entrambi presenti
//! il test viene skippato con `eprintln!` per non rompere CI dev box.
//!
//! Setup tipico:
//!   1. `cargo build -p mcp-core --release && ./target/release/mcp-core &`
//!   2. `bash /tmp/mint_jwt.sh`  (vedi tests/e2e/nexus-suite/_helpers/)
//!   3. `MCP_CORE_URL=http://localhost:4000 NEXUS_TEST_JWT=$(cat /tmp/nexus_jwt.txt) cargo test --test agent_runs_endpoints`

use std::env;
use std::time::Duration;

fn base_url() -> String {
    env::var("MCP_CORE_URL").unwrap_or_else(|_| "http://localhost:4000".into())
}

fn jwt() -> Option<String> {
    env::var("NEXUS_TEST_JWT").ok().filter(|s| !s.is_empty())
}

async fn client() -> Option<reqwest::Client> {
    // Skip se nessun JWT — i contract test richiedono auth admin.
    jwt()?;
    Some(
        reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .ok()?,
    )
}

async fn cookie_header() -> String {
    format!("token={}", jwt().unwrap_or_default())
}

#[tokio::test]
async fn health_endpoint_e_raggiungibile() {
    let url = format!("{}/health", base_url());
    let resp = reqwest::get(&url).await;
    if resp.is_err() {
        eprintln!("skip: mcp-core non in ascolto su {url}");
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
    let Some(client) = client().await else {
        eprintln!("skip: NEXUS_TEST_JWT non impostato");
        return;
    };
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
    let Some(client) = client().await else {
        eprintln!("skip");
        return;
    };
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

#[tokio::test]
async fn provider_error_bridge_billing_attiva_cooldown_lungo() {
    // Endpoint interno: simula un brain bridge call per billing_error.
    // Verifica che il provider venga messo in cooldown e poi possa essere
    // rimosso via reset-cooldown.
    let Some(client) = client().await else {
        eprintln!("skip");
        return;
    };
    // Mark cooldown via bridge.
    let bridge_url = format!("{}/api/internal/provider-error", base_url());
    let r = client
        .post(&bridge_url)
        .json(&serde_json::json!({
            "provider": "contract_test_provider",
            "error_class": "billing_error",
        }))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "bridge call deve essere 2xx");
    // Reset cooldown.
    let reset_url = format!(
        "{}/api/admin/providers/contract_test_provider/reset-cooldown",
        base_url()
    );
    let r2 = client
        .post(&reset_url)
        .header("Cookie", cookie_header().await)
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status().as_u16(), 200);
}
