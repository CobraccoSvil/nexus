//! Handler admin per la gestione degli esperimenti A/B di prompt (Fase 3).
//!
//! Endpoint disponibili:
//!   GET    /api/admin/prompt-experiments            — lista esperimenti
//!   GET    /api/admin/prompt-experiments/:id        — dettaglio singolo esperimento
//!   POST   /api/admin/prompt-experiments/:id/promote — forza promozione manuale
//!   POST   /api/admin/prompt-experiments/:id/discard — forza scarto manuale
//!   GET    /api/admin/prompt-dashboard              — riepilogo metriche per la dashboard

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde_json::{json, Value};
use sqlx::Row;

use crate::AppState;

type ApiError = (StatusCode, Json<Value>);
type ApiResult = Result<Json<Value>, ApiError>;

fn api_err(status: StatusCode, msg: impl Into<String>) -> ApiError {
    (status, Json(json!({ "error": msg.into() })))
}

// ─── Lista esperimenti ────────────────────────────────────────────────────────

pub async fn list_experiments(State(state): State<AppState>) -> ApiResult {
    let rows = sqlx::query(
        r#"
        SELECT
            id, prompt_key, baseline_version, variant_version,
            traffic_pct, status, started_at, ended_at,
            baseline_success_rate::float8 AS baseline_success_rate,
            variant_success_rate::float8  AS variant_success_rate,
            baseline_reflection_avg::float8 AS baseline_reflection_avg,
            variant_reflection_avg::float8  AS variant_reflection_avg,
            p_value::float8 AS p_value,
            decision_reason, auto_promote_enabled
        FROM prompt_ab_experiments
        ORDER BY started_at DESC
        LIMIT 100
        "#
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let experiments: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<uuid::Uuid, _>("id").to_string(),
                "prompt_key": r.get::<String, _>("prompt_key"),
                "baseline_version": r.get::<i32, _>("baseline_version"),
                "variant_version": r.get::<i32, _>("variant_version"),
                "traffic_pct": r.get::<i32, _>("traffic_pct"),
                "status": r.get::<String, _>("status"),
                "started_at": r.try_get::<chrono::NaiveDateTime, _>("started_at").ok(),
                "ended_at": r.try_get::<chrono::NaiveDateTime, _>("ended_at").ok(),
                "baseline_success_rate": r.try_get::<f64, _>("baseline_success_rate").ok(),
                "variant_success_rate": r.try_get::<f64, _>("variant_success_rate").ok(),
                "baseline_reflection_avg": r.try_get::<f64, _>("baseline_reflection_avg").ok(),
                "variant_reflection_avg": r.try_get::<f64, _>("variant_reflection_avg").ok(),
                "p_value": r.try_get::<f64, _>("p_value").ok(),
                "decision_reason": r.try_get::<String, _>("decision_reason").ok(),
                "auto_promote_enabled": r.get::<bool, _>("auto_promote_enabled"),
            })
        })
        .collect();

    Ok(Json(json!({ "experiments": experiments, "total": experiments.len() })))
}

// ─── Dettaglio singolo esperimento ────────────────────────────────────────────

pub async fn get_experiment(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult {
    let row = sqlx::query(
        r#"
        SELECT
            e.id, e.prompt_key, e.baseline_version, e.variant_version,
            e.traffic_pct, e.status, e.started_at, e.ended_at,
            e.baseline_success_rate::float8 AS baseline_success_rate,
            e.variant_success_rate::float8  AS variant_success_rate,
            e.baseline_reflection_avg::float8 AS baseline_reflection_avg,
            e.variant_reflection_avg::float8  AS variant_reflection_avg,
            e.p_value::float8 AS p_value,
            e.decision_reason, e.auto_promote_enabled,
            bl.content AS baseline_content,
            vr.content AS variant_content
        FROM prompt_ab_experiments e
        LEFT JOIN nexus_prompt_templates bl
            ON bl.key = e.prompt_key AND bl.version = e.baseline_version
        LEFT JOIN nexus_prompt_templates vr
            ON vr.key = e.prompt_key AND vr.version = e.variant_version
        WHERE e.id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_err(StatusCode::NOT_FOUND, "Esperimento non trovato"))?;

    let prompt_key: String = row.get("prompt_key");
    let baseline_version: i32 = row.get("baseline_version");
    let variant_version: i32 = row.get("variant_version");

    // Reflection stats per baseline e variante (ultime 7gg)
    let bl_stats = reflection_stats(&state.db, &prompt_key, baseline_version).await;
    let vr_stats = reflection_stats(&state.db, &prompt_key, variant_version).await;

    Ok(Json(json!({
        "id": row.get::<uuid::Uuid, _>("id").to_string(),
        "prompt_key": prompt_key,
        "baseline_version": baseline_version,
        "variant_version": variant_version,
        "traffic_pct": row.get::<i32, _>("traffic_pct"),
        "status": row.get::<String, _>("status"),
        "started_at": row.try_get::<chrono::NaiveDateTime, _>("started_at").ok(),
        "ended_at": row.try_get::<chrono::NaiveDateTime, _>("ended_at").ok(),
        "baseline_success_rate": row.try_get::<f64, _>("baseline_success_rate").ok(),
        "variant_success_rate": row.try_get::<f64, _>("variant_success_rate").ok(),
        "baseline_reflection_avg": row.try_get::<f64, _>("baseline_reflection_avg").ok(),
        "variant_reflection_avg": row.try_get::<f64, _>("variant_reflection_avg").ok(),
        "p_value": row.try_get::<f64, _>("p_value").ok(),
        "decision_reason": row.try_get::<String, _>("decision_reason").ok(),
        "auto_promote_enabled": row.get::<bool, _>("auto_promote_enabled"),
        "baseline_content": row.try_get::<String, _>("baseline_content").ok(),
        "variant_content": row.try_get::<String, _>("variant_content").ok(),
        "baseline_stats": bl_stats,
        "variant_stats": vr_stats,
    })))
}

/// Helper: recupera metriche reflection aggregate per una versione.
async fn reflection_stats(
    db: &sqlx::PgPool,
    prompt_key: &str,
    version: i32,
) -> Value {
    let row = sqlx::query(
        r#"
        SELECT
            COUNT(*)                         AS runs,
            COALESCE(AVG(score::float8), 0)  AS avg_score,
            COALESCE(MIN(score::float8), 0)  AS min_score,
            COALESCE(MAX(score::float8), 0)  AS max_score
        FROM nexus_agent_reflections
        WHERE prompt_key = $1 AND prompt_version = $2
          AND created_at >= NOW() - INTERVAL '7 days'
        "#,
    )
    .bind(prompt_key)
    .bind(version)
    .fetch_one(db)
    .await;

    match row {
        Ok(r) => json!({
            "runs": r.try_get::<i64, _>("runs").ok().unwrap_or(0),
            "avg_score": r.try_get::<f64, _>("avg_score").ok().unwrap_or(0.0),
            "min_score": r.try_get::<f64, _>("min_score").ok().unwrap_or(0.0),
            "max_score": r.try_get::<f64, _>("max_score").ok().unwrap_or(0.0),
        }),
        Err(_) => json!({ "runs": 0, "avg_score": 0.0 }),
    }
}

// ─── Azioni manuali ───────────────────────────────────────────────────────────

pub async fn force_promote(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult {
    apply_manual_decision(&state.db, id, "promoted", "Promozione manuale da admin").await
}

pub async fn force_discard(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult {
    apply_manual_decision(&state.db, id, "discarded", "Scarto manuale da admin").await
}

async fn apply_manual_decision(
    db: &sqlx::PgPool,
    experiment_id: uuid::Uuid,
    decision: &str,
    reason: &str,
) -> ApiResult {
    // Recupera dettagli esperimento
    let exp = sqlx::query(
        "SELECT prompt_key, baseline_version, variant_version, status
         FROM prompt_ab_experiments WHERE id = $1",
    )
    .bind(experiment_id)
    .fetch_optional(db)
    .await
    .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_err(StatusCode::NOT_FOUND, "Esperimento non trovato"))?;

    let exp_status: String = exp.get("status");
    let exp_prompt_key: String = exp.get("prompt_key");
    let exp_baseline_version: i32 = exp.get("baseline_version");
    let exp_variant_version: i32 = exp.get("variant_version");

    if exp_status != "running" {
        return Err(api_err(
            StatusCode::CONFLICT,
            format!("Esperimento gia' in stato '{}', non modificabile", exp_status),
        ));
    }

    // Aggiorna stato esperimento
    sqlx::query(
        "UPDATE prompt_ab_experiments
         SET status = $1, ended_at = NOW(), decision_reason = $2
         WHERE id = $3",
    )
    .bind(decision)
    .bind(reason)
    .bind(experiment_id)
    .execute(db)
    .await
    .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if decision == "promoted" {
        // Disattiva baseline
        sqlx::query(
            "UPDATE nexus_prompt_templates SET is_active = FALSE
             WHERE key = $1 AND version = $2",
        )
        .bind(&exp_prompt_key)
        .bind(exp_baseline_version)
        .execute(db)
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        // Attiva variante
        sqlx::query(
            "UPDATE nexus_prompt_templates
             SET is_active = TRUE, experimental = FALSE
             WHERE key = $1 AND version = $2",
        )
        .bind(&exp_prompt_key)
        .bind(exp_variant_version)
        .execute(db)
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    tracing::info!(
        "experiments: {} manuale su esperimento {} per '{}'",
        decision, experiment_id, exp_prompt_key,
    );

    Ok(Json(json!({
        "ok": true,
        "experiment_id": experiment_id,
        "decision": decision,
        "prompt_key": exp_prompt_key,
    })))
}

// ─── Dashboard riepilogo ──────────────────────────────────────────────────────

pub async fn prompt_dashboard(State(state): State<AppState>) -> ApiResult {
    // Metriche per prompt agente (ultimi 7 giorni)
    let prompt_metrics = sqlx::query(
        r#"
        SELECT
            t.key                                               AS prompt_key,
            t.version                                          AS prompt_version,
            t.schema_type,
            t.experimental,
            COALESCE(AVG(r.score::float8), NULL)               AS avg_reflection_score,
            COUNT(r.id)                                        AS reflection_runs,
            NULL::float8                                       AS feedback_positive_rate,
            0::bigint                                              AS feedback_count
        FROM nexus_prompt_templates t
        LEFT JOIN nexus_agent_reflections r
            ON r.prompt_key = t.key
           AND r.prompt_version = t.version
           AND r.created_at >= NOW() - INTERVAL '7 days'
        WHERE t.is_active = TRUE
          AND t.key LIKE 'agent.%'
        GROUP BY t.key, t.version, t.schema_type, t.experimental
        ORDER BY avg_reflection_score ASC NULLS LAST
        "#
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Esperimenti attivi
    let active_experiments = sqlx::query(
        r#"
        SELECT id, prompt_key, baseline_version, variant_version,
               traffic_pct, status, started_at,
               baseline_success_rate::float8 AS baseline_success_rate,
               variant_success_rate::float8  AS variant_success_rate
        FROM prompt_ab_experiments
        WHERE status = 'running'
        ORDER BY started_at DESC
        "#
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Reflection score medio globale ultimi 7gg
    let global_avg: Option<f64> = sqlx::query_scalar::<_, Option<f64>>(
        "SELECT AVG(score::float8) FROM nexus_agent_reflections
         WHERE created_at >= NOW() - INTERVAL '7 days'"
    )
    .fetch_one(&state.db)
    .await
    .unwrap_or(None);

    let prompts: Vec<Value> = prompt_metrics.iter().map(|r| json!({
        "prompt_key": r.get::<String, _>("prompt_key"),
        "prompt_version": r.get::<i32, _>("prompt_version"),
        "schema_type": r.try_get::<String, _>("schema_type").ok(),
        "experimental": r.get::<bool, _>("experimental"),
        "avg_reflection_score": r.try_get::<f64, _>("avg_reflection_score").ok(),
        "reflection_runs": r.try_get::<i64, _>("reflection_runs").ok().unwrap_or(0),
        "feedback_positive_rate": r.try_get::<f64, _>("feedback_positive_rate").ok(),
        "feedback_count": r.try_get::<i64, _>("feedback_count").ok().unwrap_or(0),
    })).collect();

    let experiments: Vec<Value> = active_experiments.iter().map(|r| json!({
        "id": r.get::<uuid::Uuid, _>("id").to_string(),
        "prompt_key": r.get::<String, _>("prompt_key"),
        "baseline_version": r.get::<i32, _>("baseline_version"),
        "variant_version": r.get::<i32, _>("variant_version"),
        "traffic_pct": r.get::<i32, _>("traffic_pct"),
        "status": r.get::<String, _>("status"),
        "started_at": r.try_get::<chrono::NaiveDateTime, _>("started_at").ok(),
        "baseline_success_rate": r.try_get::<f64, _>("baseline_success_rate").ok(),
        "variant_success_rate": r.try_get::<f64, _>("variant_success_rate").ok(),
    })).collect();

    Ok(Json(json!({
        "prompts": prompts,
        "active_experiments": experiments,
        "global_reflection_avg_7d": global_avg,
        "total_prompts": prompts.len(),
        "running_experiments": experiments.len(),
    })))
}
