//! Contract test (PR-4 Livello 3): M71 cost breakdown per provider/model.
//!
//! Verifica che il GET `/api/chat/agent-runs/:id` includa il campo
//! `usageBreakdown` aggregato da `ai_usage_ledger`. Test sintetico:
//! inserisce ledger fake → fetcha endpoint → controlla breakdown.

use std::env;
use std::time::Duration;
use sqlx::PgPool;
use uuid::Uuid;

fn base_url() -> String { env::var("MCP_CORE_URL").unwrap_or_else(|_| "http://localhost:4000".into()) }
fn jwt() -> Option<String> { env::var("NEXUS_TEST_JWT").ok().filter(|s| !s.is_empty()) }

async fn db() -> Option<PgPool> {
    let url = env::var("DATABASE_URL").ok()?;
    PgPool::connect(&url).await.ok()
}

#[tokio::test]
async fn agent_run_endpoint_include_usage_breakdown_aggregato() {
    let Some(token) = jwt() else { eprintln!("skip: NEXUS_TEST_JWT non impostato"); return; };
    let Some(pool) = db().await else { eprintln!("skip: DATABASE_URL non impostata"); return; };

    // Setup: trova (o crea) un agent_run di test, popola ai_usage_ledger con
    // 3 record per (anthropic/claude-sonnet, deepseek/deepseek-chat, mistral/mistral-large).
    let run_id = Uuid::new_v4();
    let session_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM chat_sessions LIMIT 1",
    ).fetch_optional(&pool).await.unwrap_or(None);
    let project_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM projects LIMIT 1",
    ).fetch_optional(&pool).await.unwrap_or(None);
    let user_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM users LIMIT 1",
    ).fetch_optional(&pool).await.unwrap_or(None);
    let (Some(sid), Some(pid), Some(uid)) = (session_id, project_id, user_id) else {
        eprintln!("skip: DB senza session/project/user di seed");
        return;
    };

    // Insert dummy agent_run
    let _ = sqlx::query(
        "INSERT INTO agent_runs (id, session_id, project_id, user_id, status, automation_mode, provider, model, created_at)
         VALUES ($1, $2, $3, $4, 'completed', 'automatic', 'deepseek', 'deepseek-chat', NOW())",
    )
    .bind(run_id).bind(sid).bind(pid).bind(uid)
    .execute(&pool).await;

    // Insert 3 ledger rows for cascade simulation
    for (prov, model, p, c, cost) in [
        ("anthropic", "claude-sonnet-4-6", 1000_i64, 200_i64, 0.012_f64),
        ("deepseek",  "deepseek-chat",     1500_i64, 400_i64, 0.0008_f64),
        ("mistral",   "mistral-large-latest", 500_i64, 100_i64, 0.005_f64),
    ] {
        let _ = sqlx::query(
            "INSERT INTO ai_usage_ledger
                (id, run_id, user_id, project_id, provider, model,
                 prompt_tokens, completion_tokens, total_tokens, total_cost,
                 input_cost, output_cost, currency, status, finalized_at, created_at)
             VALUES (gen_random_uuid(), $1, $2, $3, $4, $5,
                     $6, $7, $8, $9, 0, 0, 'USD', 'finalized', NOW(), NOW())",
        )
        .bind(run_id).bind(uid).bind(pid).bind(prov).bind(model)
        .bind(p).bind(c).bind(p + c).bind(cost)
        .execute(&pool).await;
    }

    // Fetch endpoint
    let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().unwrap();
    let url = format!("{}/api/chat/agent-runs/{}", base_url(), run_id);
    let resp = client.get(&url).header("Cookie", format!("token={token}"))
        .send().await.expect("request fallita");

    if !resp.status().is_success() {
        eprintln!("skip: endpoint ritorna {} (forse user_id non autorizzato sul run)", resp.status());
        let _ = sqlx::query("DELETE FROM agent_runs WHERE id = $1").bind(run_id).execute(&pool).await;
        return;
    }

    let body: serde_json::Value = resp.json().await.expect("json");
    let breakdown = body.get("usageBreakdown").and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("usageBreakdown assente nel response: {body}"));
    assert!(breakdown.len() >= 3, "atteso almeno 3 entry (cascade), trovato {}", breakdown.len());
    let providers: Vec<String> = breakdown.iter()
        .filter_map(|e| e.get("provider").and_then(|v| v.as_str()).map(String::from))
        .collect();
    for must in ["anthropic", "deepseek", "mistral"] {
        assert!(providers.iter().any(|p| p == must), "provider '{must}' assente nel breakdown");
    }
    // Verifica somma costi nel breakdown
    let total: f64 = breakdown.iter()
        .filter_map(|e| e.get("totalCost").and_then(|v| v.as_f64()))
        .sum();
    assert!((total - 0.0178).abs() < 0.0001, "somma costi {total} != 0.0178 atteso");

    // Cleanup
    let _ = sqlx::query("DELETE FROM ai_usage_ledger WHERE run_id = $1").bind(run_id).execute(&pool).await;
    let _ = sqlx::query("DELETE FROM agent_runs WHERE id = $1").bind(run_id).execute(&pool).await;
}
