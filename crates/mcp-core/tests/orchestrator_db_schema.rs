//! Contract test (PR-4 Livello 3): schema DB delle tabelle del piano.
//!
//! Verifica che le tabelle e i prompt keys necessari a PR-1/PR-2/PR-3 esistano
//! in DB. Salta se `DATABASE_URL` non impostata, ma lo skip passa dal punto
//! unico `support::salta`: prima quattro dei cinque stampavano il solo `"skip"`.
//!
//! Eseguire con:
//!   DATABASE_URL=postgres://nexus:nexus@localhost:5433/nexus cargo test --test orchestrator_db_schema

use sqlx::{PgPool, Row};
use nexus_test_preconditions::db_o_salta;

#[tokio::test]
async fn tabelle_plan_act_verify_esistono() {
    let Some(pool) = db_o_salta().await else { return };
    // Tabelle di PIATTAFORMA: vivono nel meta-DB (DATABASE_URL).
    let attese_meta = [
        "nexus_subagent_definitions",
        "nexus_project_instructions",
        "nexus_security_audit",
    ];
    for t in attese_meta {
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

/// Tabelle del dominio run: migrate ai DB-progetto (decommissionate nel meta
/// dalla 0507). La verifica APPLICA il set `db/migrations/project` a un DB
/// effimero e interroga lo schema risultante.
///
/// Prima cercava `CREATE TABLE <nome>` nel testo dei file: quel controllo
/// imitava il parser invece di chiedere al DB (regola O), quindi passava anche
/// se la CREATE era dentro un blocco che non veniva mai eseguito e falliva su
/// una tabella creata in modo diverso da come la stringa se l'aspettava. Il
/// dubbio storico ("un DB-progetto effimero richiederebbe credenziali del
/// cluster app") non regge: il migrator gira su qualunque DB vuoto, ed e' lo
/// stesso che la produzione applica a `<slug>_nexus`.
#[sqlx::test(migrator = "nexus_test_schema::PROJECT_MIGRATOR")]
async fn tabelle_dominio_run_esistono_dopo_le_migrazioni(pool: PgPool) {
    let attese_project = [
        "nexus_agent_plans",
        "nexus_agent_todos",
        "nexus_agent_verifier_runs",
        "nexus_subagent_runs",
    ];
    for t in attese_project {
        let row = sqlx::query(
            "SELECT 1 AS x FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_name = $1",
        )
        .bind(t)
        .fetch_optional(&pool)
        .await
        .expect("query");
        assert!(
            row.is_some(),
            "tabella '{t}' assente dopo db/migrations/project - dominio run incompleto"
        );
    }
}

#[tokio::test]
async fn prompt_keys_orchestrator_seedati() {
    let Some(pool) = db_o_salta().await else { return };
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
    let Some(pool) = db_o_salta().await else { return };
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
    let Some(pool) = db_o_salta().await else { return };
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
    let Some(pool) = db_o_salta().await else { return };
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
