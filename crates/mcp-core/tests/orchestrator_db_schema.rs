//! Contract test (PR-4 Livello 3): schema DB delle tabelle del piano.
//!
//! Verifica che le tabelle e i prompt keys necessari a PR-1/PR-2/PR-3 esistano
//! in DB. Skip se `DATABASE_URL` non impostata.
//!
//! Eseguire con:
//!   DATABASE_URL=postgres://nexus:nexus@localhost:5433/nexus cargo test --test orchestrator_db_schema

use sqlx::{PgPool, Row};
use std::env;

async fn pool_or_skip() -> Option<PgPool> {
    let url = env::var("DATABASE_URL").ok()?;
    PgPool::connect(&url).await.ok()
}

#[tokio::test]
async fn tabelle_plan_act_verify_esistono() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("skip: DATABASE_URL non impostata");
        return;
    };
    let attese = [
        "nexus_agent_plans",
        "nexus_agent_todos",
        "nexus_agent_verifier_runs",
        "nexus_subagent_definitions",
        "nexus_subagent_runs",
        "nexus_project_instructions",
        "nexus_security_audit",
    ];
    for t in attese {
        let row = sqlx::query("SELECT 1 AS x FROM information_schema.tables WHERE table_name = $1")
            .bind(t)
            .fetch_optional(&pool)
            .await
            .expect("query");
        assert!(
            row.is_some(),
            "tabella '{t}' NON ESISTE - applicare migration"
        );
    }
}

#[tokio::test]
async fn prompt_keys_orchestrator_seedati() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("skip");
        return;
    };
    let attese = [
        "agent.planner.base",
        "agent.plan_revision.tpl",
        "agent.todo_reminder.tpl",
        "agent.verifier.base",
        "verification.failed_block",
        "subagent.plan.base",
        "subagent.explore.base",
        "subagent.implement.base",
        "subagent.verify.base",
        "subagent.review.base",
        "subagent.result_block",
        "agent.clarifying.detect",
        "agent.clarifying.defaults_applied",
        "system.project_instructions_block",
        "system.available_subagents_block",
    ];
    for k in attese {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT key FROM nexus_prompt_templates WHERE key = $1 AND is_active = true",
        )
        .bind(k)
        .fetch_optional(&pool)
        .await
        .expect("query");
        assert!(row.is_some(), "prompt key '{k}' assente o disattivato");
    }
}

#[tokio::test]
async fn settings_orchestrator_eligibility_consistente() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("skip");
        return;
    };
    // Numeric/CSV settings: presenti e parseabili.
    let pairs = [
        ("orchestrator.plan_phase_enabled", "bool"),
        ("orchestrator.verifier_enabled", "bool"),
        ("orchestrator.subagents_enabled", "bool"),
        ("orchestrator.max_verify_cycles", "int"),
        ("orchestrator.max_plan_revisions", "int"),
        ("orchestrator.todo_reminder_every_n_steps", "int"),
        ("orchestrator.max_parallel_subagents", "int"),
        ("orchestrator.subagent_max_depth", "int"),
        ("orchestrator.subagent_cost_cap_per_run_usd", "float"),
        ("orchestrator.plan_intents", "csv"),
        ("orchestrator.plan_behavior_modes", "csv"),
        ("orchestrator.subagent_kinds_whitelist", "csv"),
    ];
    for (key, kind) in pairs {
        let row: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key = $1")
            .bind(key)
            .fetch_optional(&pool)
            .await
            .expect("query");
        let v = row.unwrap_or_else(|| panic!("setting '{key}' mancante")).0;
        match kind {
            "bool" => assert!(v == "true" || v == "false", "{key}: '{v}' non bool"),
            "int" => {
                v.parse::<i64>()
                    .unwrap_or_else(|_| panic!("{key}: '{v}' non int"));
            }
            "float" => {
                v.parse::<f64>()
                    .unwrap_or_else(|_| panic!("{key}: '{v}' non float"));
            }
            "csv" => assert!(v.contains(',') || !v.is_empty(), "{key} csv vuota"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn subagent_kinds_seedati_con_purpose_validi() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("skip");
        return;
    };
    let rows = sqlx::query(
        "SELECT kind, model_purpose FROM nexus_subagent_definitions WHERE is_enabled = true",
    )
    .fetch_all(&pool)
    .await
    .expect("query");
    let kinds: Vec<String> = rows
        .iter()
        .map(|r| r.try_get("kind").unwrap_or_default())
        .collect();
    assert!(
        kinds.len() >= 5,
        "almeno 5 kind seedati, trovati: {:?}",
        kinds
    );
    for must in ["plan", "explore", "implement", "verify", "review"] {
        assert!(kinds.iter().any(|k| k == must), "kind '{must}' assente");
    }
    // Ogni purpose deve esistere in nexus_purpose_model
    for r in &rows {
        let purpose: String = r.try_get("model_purpose").unwrap_or_default();
        let exists: Option<(String,)> =
            sqlx::query_as("SELECT provider FROM nexus_purpose_model WHERE purpose = $1")
                .bind(&purpose)
                .fetch_optional(&pool)
                .await
                .expect("query");
        assert!(
            exists.is_some(),
            "purpose '{purpose}' del kind '{}' non esiste in nexus_purpose_model",
            r.try_get::<String, _>("kind").unwrap_or_default()
        );
    }
}

#[tokio::test]
async fn ai_usage_ledger_schema_supporta_breakdown_m71() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("skip");
        return;
    };
    let cols = [
        "provider",
        "model",
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
        "total_cost",
        "run_id",
        "status",
    ];
    for c in cols {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT column_name FROM information_schema.columns
             WHERE table_name = 'ai_usage_ledger' AND column_name = $1",
        )
        .bind(c)
        .fetch_optional(&pool)
        .await
        .expect("query");
        assert!(
            row.is_some(),
            "colonna ai_usage_ledger.{c} mancante (rompe M71 breakdown)"
        );
    }
}
