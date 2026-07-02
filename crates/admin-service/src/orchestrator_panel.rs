//! PR-4 — Endpoint admin per il pannello Orchestrator (Plan/Act/Verify + Sub-agents).
//!
//! Routes esposti (tutti sotto `/api/admin/orchestrator`):
//!   * GET    /plans?project_id=&limit=   — lista plan recenti (con todos/verifier rollup)
//!   * GET    /plans/:run_id             — dettaglio plan + todos + verifier_runs + subagent_runs tree
//!   * GET    /subagents/definitions      — lista kind sub-agent (DB)
//!   * POST   /subagents/definitions      — crea kind custom
//!   * PATCH  /subagents/definitions/:k   — aggiorna
//!   * DELETE /subagents/definitions/:k   — disabilita (soft delete via is_enabled=false)
//!   * GET    /subagents/runs?parent_run_id=&kind=&project_id=&limit=  — drill-down
//!
//! Tutti i metodi richiedono role=admin via middleware require_admin del main.rs.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::AppState;

// ── Routing pool per-progetto ───────────────────────────────────────────────
// Separazione DB: nexus_agent_plans/todos/verifier_runs e nexus_subagent_runs
// vivono nei DB-progetto (nel meta sono decommissionate, mig 0507). Le viste
// GLOBALI di questo pannello aggregano iterando i DB-progetto; a flag OFF si
// interroga il solo meta come prima del cutover (query storica invariata).

/// Pool del dominio run da interrogare. A flag OFF: il solo meta. A flag ON:
/// un pool per progetto (o il solo progetto richiesto). Un DB non ancora
/// provisionato non ha dati (skip silenzioso); un DB irraggiungibile degrada
/// con WARN senza azzerare gli altri (pattern detect_all_port_bindings).
async fn run_domain_pools(state: &AppState, only: Option<Uuid>) -> Vec<sqlx::PgPool> {
    if !nexus_project_pools::separation_enabled(&state.db).await {
        return vec![state.db.clone()];
    }
    let ids = match only {
        Some(pid) => vec![pid],
        None => nexus_project_pools::list_project_ids(&state.db).await,
    };
    let mut pools = Vec::with_capacity(ids.len());
    for pid in ids {
        match nexus_project_pools::project_data_pool(&state.db, pid).await {
            Ok(p) => pools.push(p),
            Err(nexus_project_pools::ProjectPoolError::NotProvisioned(_)) => {}
            Err(e) => tracing::warn!(
                project_id = %pid,
                error = %e,
                "orchestrator_panel: pool progetto non risolvibile, progetto saltato"
            ),
        }
    }
    pools
}

// ── Plans ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct PlansQuery {
    pub project_id: Option<Uuid>,
    pub limit: Option<i64>,
}

pub async fn list_plans(
    State(state): State<AppState>,
    Query(q): Query<PlansQuery>,
) -> Result<Json<Value>, axum::http::StatusCode> {
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    // (created_at per il merge-sort cross-progetto, payload JSON)
    let mut plans: Vec<(Option<DateTime<Utc>>, Value)> = Vec::new();
    for pool in run_domain_pools(&state, q.project_id).await {
        let rows = if let Some(pid) = q.project_id {
            sqlx::query(
                r#"SELECT p.run_id::text, p.project_id::text, p.planner_model, p.created_at,
                          p.approved_at, p.score,
                          (SELECT COUNT(*) FROM nexus_agent_todos t WHERE t.run_id = p.run_id) AS todos_total,
                          (SELECT COUNT(*) FROM nexus_agent_todos t WHERE t.run_id = p.run_id AND t.status='completed') AS todos_done,
                          (SELECT COUNT(*) FROM nexus_agent_verifier_runs vr WHERE vr.run_id = p.run_id) AS verifier_runs,
                          (SELECT COUNT(*) FROM nexus_subagent_runs sr WHERE sr.parent_run_id::text = p.run_id::text) AS subagent_runs
                   FROM nexus_agent_plans p
                   WHERE p.project_id = $1
                   ORDER BY p.created_at DESC LIMIT $2"#,
            )
            .bind(pid)
            .bind(limit)
            .fetch_all(&pool)
            .await
        } else {
            sqlx::query(
                r#"SELECT p.run_id::text, p.project_id::text, p.planner_model, p.created_at,
                          p.approved_at, p.score,
                          (SELECT COUNT(*) FROM nexus_agent_todos t WHERE t.run_id = p.run_id) AS todos_total,
                          (SELECT COUNT(*) FROM nexus_agent_todos t WHERE t.run_id = p.run_id AND t.status='completed') AS todos_done,
                          (SELECT COUNT(*) FROM nexus_agent_verifier_runs vr WHERE vr.run_id = p.run_id) AS verifier_runs,
                          (SELECT COUNT(*) FROM nexus_subagent_runs sr WHERE sr.parent_run_id::text = p.run_id::text) AS subagent_runs
                   FROM nexus_agent_plans p
                   ORDER BY p.created_at DESC LIMIT $1"#,
            )
            .bind(limit)
            .fetch_all(&pool)
            .await
        }
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

        plans.extend(rows.iter().map(|r| {
            let created_at = r.try_get::<DateTime<Utc>, _>("created_at").ok();
            let payload = json!({
                "runId": r.try_get::<String, _>("run_id").unwrap_or_default(),
                "projectId": r.try_get::<String, _>("project_id").unwrap_or_default(),
                "plannerModel": r.try_get::<Option<String>, _>("planner_model").ok().flatten(),
                "createdAt": created_at.map(|v| v.to_rfc3339()),
                "approvedAt": r.try_get::<Option<DateTime<Utc>>, _>("approved_at").ok().flatten().map(|v| v.to_rfc3339()),
                "score": r.try_get::<Option<f64>, _>("score").ok().flatten(),
                "todosTotal": r.try_get::<i64, _>("todos_total").unwrap_or(0),
                "todosDone": r.try_get::<i64, _>("todos_done").unwrap_or(0),
                "verifierRuns": r.try_get::<i64, _>("verifier_runs").unwrap_or(0),
                "subagentRuns": r.try_get::<i64, _>("subagent_runs").unwrap_or(0),
            });
            (created_at, payload)
        }));
    }
    // Merge cross-progetto: ordina per created_at DESC e riapplica il limit.
    plans.sort_by(|a, b| b.0.cmp(&a.0));
    plans.truncate(limit as usize);
    let plans: Vec<Value> = plans.into_iter().map(|(_, v)| v).collect();
    Ok(Json(json!({"plans": plans})))
}

pub async fn get_plan(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Result<Json<Value>, axum::http::StatusCode> {
    // Il run_id non porta il progetto: si cerca il plan nei DB del dominio run
    // (a flag OFF la lista e' il solo meta -> query storica). Trovato il plan,
    // todos/verifier/subagent si leggono dallo STESSO pool.
    let mut found: Option<(sqlx::PgPool, sqlx::postgres::PgRow)> = None;
    for pool in run_domain_pools(&state, None).await {
        let plan = sqlx::query(
            r#"SELECT run_id::text, project_id::text, planner_model, acceptance_criteria,
                      approved_at, approved_by::text, score, created_at
               FROM nexus_agent_plans WHERE run_id::text = $1"#,
        )
        .bind(&run_id)
        .fetch_optional(&pool)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        if let Some(row) = plan {
            found = Some((pool, row));
            break;
        }
    }
    let (pool, plan) = found.ok_or(axum::http::StatusCode::NOT_FOUND)?;

    let todos = sqlx::query(
        r#"SELECT id::text, seq, content, status, priority, verify_failures, updated_at
           FROM nexus_agent_todos WHERE run_id::text = $1 ORDER BY seq"#,
    )
    .bind(&run_id)
    .fetch_all(&pool)
    .await
    .unwrap_or_default();

    let verifier_runs = sqlx::query(
        r#"SELECT id::text, todo_id::text, cycle, criteria_results, passed, duration_ms, created_at
           FROM nexus_agent_verifier_runs WHERE run_id::text = $1 ORDER BY created_at"#,
    )
    .bind(&run_id)
    .fetch_all(&pool)
    .await
    .unwrap_or_default();

    let subagent_runs = sqlx::query(
        r#"SELECT id::text, parent_run_id::text, kind, task_description, status,
                  iterations, tokens_prompt, tokens_completion, cost_usd, depth, source,
                  created_at, completed_at
           FROM nexus_subagent_runs WHERE parent_run_id::text = $1 ORDER BY created_at"#,
    )
    .bind(&run_id)
    .fetch_all(&pool)
    .await
    .unwrap_or_default();

    Ok(Json(json!({
        "runId": plan.try_get::<String, _>("run_id").unwrap_or_default(),
        "projectId": plan.try_get::<String, _>("project_id").unwrap_or_default(),
        "plannerModel": plan.try_get::<Option<String>, _>("planner_model").ok().flatten(),
        "acceptanceCriteria": plan.try_get::<Option<Value>, _>("acceptance_criteria").ok().flatten(),
        "approvedAt": plan.try_get::<Option<DateTime<Utc>>, _>("approved_at").ok().flatten().map(|v| v.to_rfc3339()),
        "approvedBy": plan.try_get::<Option<String>, _>("approved_by").ok().flatten(),
        "score": plan.try_get::<Option<f64>, _>("score").ok().flatten(),
        "createdAt": plan.try_get::<DateTime<Utc>, _>("created_at").ok().map(|v| v.to_rfc3339()),
        "todos": todos.iter().map(|r| json!({
            "id": r.try_get::<String, _>("id").unwrap_or_default(),
            "seq": r.try_get::<i32, _>("seq").unwrap_or(0),
            "content": r.try_get::<String, _>("content").unwrap_or_default(),
            "status": r.try_get::<String, _>("status").unwrap_or_default(),
            "priority": r.try_get::<String, _>("priority").unwrap_or_default(),
            "verifyFailures": r.try_get::<i32, _>("verify_failures").unwrap_or(0),
            "updatedAt": r.try_get::<DateTime<Utc>, _>("updated_at").ok().map(|v| v.to_rfc3339()),
        })).collect::<Vec<_>>(),
        "verifierRuns": verifier_runs.iter().map(|r| json!({
            "id": r.try_get::<String, _>("id").unwrap_or_default(),
            "todoId": r.try_get::<Option<String>, _>("todo_id").ok().flatten(),
            "cycle": r.try_get::<i32, _>("cycle").unwrap_or(0),
            "criteriaResults": r.try_get::<Value, _>("criteria_results").unwrap_or(json!([])),
            "passed": r.try_get::<bool, _>("passed").unwrap_or(false),
            "durationMs": r.try_get::<Option<i32>, _>("duration_ms").ok().flatten(),
            "createdAt": r.try_get::<DateTime<Utc>, _>("created_at").ok().map(|v| v.to_rfc3339()),
        })).collect::<Vec<_>>(),
        "subagentRuns": subagent_runs.iter().map(|r| json!({
            "id": r.try_get::<String, _>("id").unwrap_or_default(),
            "parentRunId": r.try_get::<Option<String>, _>("parent_run_id").ok().flatten(),
            "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
            "task": r.try_get::<String, _>("task_description").unwrap_or_default(),
            "status": r.try_get::<String, _>("status").unwrap_or_default(),
            "iterations": r.try_get::<i32, _>("iterations").unwrap_or(0),
            "tokensPrompt": r.try_get::<i32, _>("tokens_prompt").unwrap_or(0),
            "tokensCompletion": r.try_get::<i32, _>("tokens_completion").unwrap_or(0),
            "costUsd": r.try_get::<f64, _>("cost_usd").unwrap_or(0.0),
            "depth": r.try_get::<i32, _>("depth").unwrap_or(1),
            "source": r.try_get::<String, _>("source").unwrap_or_else(|_| "db".into()),
            "createdAt": r.try_get::<DateTime<Utc>, _>("created_at").ok().map(|v| v.to_rfc3339()),
            "completedAt": r.try_get::<Option<DateTime<Utc>>, _>("completed_at").ok().flatten().map(|v| v.to_rfc3339()),
        })).collect::<Vec<_>>(),
    })))
}

// ── Subagent definitions ───────────────────────────────────────────────────

pub async fn list_subagent_definitions(
    State(state): State<AppState>,
) -> Result<Json<Value>, axum::http::StatusCode> {
    let rows = sqlx::query(
        "SELECT kind, description, prompt_key, tool_whitelist, model_purpose,
                max_iterations, timeout_s, is_background, is_enabled, updated_at
         FROM nexus_subagent_definitions ORDER BY kind",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let defs: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
                "description": r.try_get::<Option<String>, _>("description").ok().flatten(),
                "promptKey": r.try_get::<String, _>("prompt_key").unwrap_or_default(),
                "toolWhitelist": r.try_get::<Vec<String>, _>("tool_whitelist").unwrap_or_default(),
                "modelPurpose": r.try_get::<String, _>("model_purpose").unwrap_or_default(),
                "maxIterations": r.try_get::<i32, _>("max_iterations").unwrap_or(25),
                "timeoutS": r.try_get::<i32, _>("timeout_s").unwrap_or(300),
                "isBackground": r.try_get::<bool, _>("is_background").unwrap_or(false),
                "isEnabled": r.try_get::<bool, _>("is_enabled").unwrap_or(true),
                "updatedAt": r.try_get::<DateTime<Utc>, _>("updated_at").ok().map(|v| v.to_rfc3339()),
            })
        })
        .collect();
    Ok(Json(json!({"definitions": defs})))
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SubagentDefBody {
    pub kind: String,
    pub description: Option<String>,
    pub prompt_key: String,
    pub tool_whitelist: Vec<String>,
    pub model_purpose: String,
    pub max_iterations: Option<i32>,
    pub timeout_s: Option<i32>,
    pub is_background: Option<bool>,
    pub is_enabled: Option<bool>,
}

pub async fn upsert_subagent_definition(
    State(state): State<AppState>,
    Json(body): Json<SubagentDefBody>,
) -> Result<Json<Value>, axum::http::StatusCode> {
    sqlx::query(
        r#"INSERT INTO nexus_subagent_definitions
              (kind, description, prompt_key, tool_whitelist, model_purpose,
               max_iterations, timeout_s, is_background, is_enabled, updated_at)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9, NOW())
           ON CONFLICT (kind) DO UPDATE SET
              description = EXCLUDED.description,
              prompt_key = EXCLUDED.prompt_key,
              tool_whitelist = EXCLUDED.tool_whitelist,
              model_purpose = EXCLUDED.model_purpose,
              max_iterations = EXCLUDED.max_iterations,
              timeout_s = EXCLUDED.timeout_s,
              is_background = EXCLUDED.is_background,
              is_enabled = EXCLUDED.is_enabled,
              updated_at = NOW()"#,
    )
    .bind(&body.kind)
    .bind(&body.description)
    .bind(&body.prompt_key)
    .bind(&body.tool_whitelist)
    .bind(&body.model_purpose)
    .bind(body.max_iterations.unwrap_or(25))
    .bind(body.timeout_s.unwrap_or(300))
    .bind(body.is_background.unwrap_or(false))
    .bind(body.is_enabled.unwrap_or(true))
    .execute(&state.db)
    .await
    .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"ok": true, "kind": body.kind})))
}

pub async fn delete_subagent_definition(
    State(state): State<AppState>,
    Path(kind): Path<String>,
) -> Result<Json<Value>, axum::http::StatusCode> {
    sqlx::query("UPDATE nexus_subagent_definitions SET is_enabled = false, updated_at = NOW() WHERE kind = $1")
        .bind(&kind)
        .execute(&state.db)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"ok": true, "kind": kind, "soft_deleted": true})))
}

#[derive(Debug, Deserialize)]
pub struct SubagentRunsQuery {
    pub parent_run_id: Option<String>,
    pub kind: Option<String>,
    pub project_id: Option<Uuid>,
    pub limit: Option<i64>,
}

pub async fn list_subagent_runs(
    State(state): State<AppState>,
    Query(q): Query<SubagentRunsQuery>,
) -> Result<Json<Value>, axum::http::StatusCode> {
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let mut sql = String::from(
        "SELECT id::text, parent_run_id::text, project_id::text, kind, task_description,
                status, iterations, tokens_prompt, tokens_completion, cost_usd, depth, source,
                created_at, completed_at
         FROM nexus_subagent_runs WHERE 1=1",
    );
    if q.parent_run_id.is_some() {
        sql.push_str(" AND parent_run_id::text = $1");
    } else if q.kind.is_some() {
        sql.push_str(" AND kind = $1");
    } else if q.project_id.is_some() {
        sql.push_str(" AND project_id = $1");
    }
    sql.push_str(" ORDER BY created_at DESC LIMIT ");
    sql.push_str(&limit.to_string());

    // Con project_id si interroga il solo DB di quel progetto; gli altri filtri
    // (parent_run_id, kind, nessuno) sono cross-progetto -> aggregazione.
    let mut runs: Vec<(Option<DateTime<Utc>>, Value)> = Vec::new();
    for pool in run_domain_pools(&state, q.project_id).await {
        let mut query = sqlx::query(&sql);
        if let Some(p) = &q.parent_run_id {
            query = query.bind(p);
        } else if let Some(k) = &q.kind {
            query = query.bind(k);
        } else if let Some(pid) = q.project_id {
            query = query.bind(pid);
        }
        let rows = query
            .fetch_all(&pool)
            .await
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

        runs.extend(rows.iter().map(|r| {
            let created_at = r.try_get::<DateTime<Utc>, _>("created_at").ok();
            let payload = json!({
                "id": r.try_get::<String, _>("id").unwrap_or_default(),
                "parentRunId": r.try_get::<Option<String>, _>("parent_run_id").ok().flatten(),
                "projectId": r.try_get::<Option<String>, _>("project_id").ok().flatten(),
                "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
                "task": r.try_get::<String, _>("task_description").unwrap_or_default(),
                "status": r.try_get::<String, _>("status").unwrap_or_default(),
                "iterations": r.try_get::<i32, _>("iterations").unwrap_or(0),
                "tokensPrompt": r.try_get::<i32, _>("tokens_prompt").unwrap_or(0),
                "tokensCompletion": r.try_get::<i32, _>("tokens_completion").unwrap_or(0),
                "costUsd": r.try_get::<f64, _>("cost_usd").unwrap_or(0.0),
                "depth": r.try_get::<i32, _>("depth").unwrap_or(1),
                "source": r.try_get::<String, _>("source").unwrap_or_else(|_| "db".into()),
                "createdAt": created_at.map(|v| v.to_rfc3339()),
                "completedAt": r.try_get::<Option<DateTime<Utc>>, _>("completed_at").ok().flatten().map(|v| v.to_rfc3339()),
            });
            (created_at, payload)
        }));
    }
    // Merge cross-progetto: ordina per created_at DESC e riapplica il limit.
    runs.sort_by(|a, b| b.0.cmp(&a.0));
    runs.truncate(limit as usize);
    let runs: Vec<Value> = runs.into_iter().map(|(_, v)| v).collect();
    Ok(Json(json!({"runs": runs})))
}
