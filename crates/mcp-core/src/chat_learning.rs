use std::collections::{HashMap, HashSet};
use std::time::Duration;

use axum::{
    extract::{Extension, Path as AxumPath, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Local, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{auth::Claims, vector_memory, AppState};

// ── Shared types and helpers — re-exported from nexus-types crate ──

pub use nexus_types::{
    api_error, ensure_project_access, parse_project_id, parse_user_id, ApiError, ApiResult,
};

pub(crate) fn normalize_text(input: &str) -> String {
    input
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub(crate) fn hash_hint(project_id: Uuid, intent: &str, text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(project_id.as_bytes());
    hasher.update(b":");
    hasher.update(intent.as_bytes());
    hasher.update(b":");
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

// ── Learning-specific types ──

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFeedbackRequest {
    pub status: String,
    pub review_note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrainProjectRequest {
    pub intent: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProjectLearningConfigRequest {
    pub enabled: Option<bool>,
    pub prompt_corrections_enabled: Option<bool>,
    pub auto_apply_max_changes_per_day: Option<i32>,
    pub feedback_threshold: Option<i32>,
    pub feedback_window_days: Option<i32>,
    pub min_confidence: Option<f64>,
    pub rollback_window_hours: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactVectorRequest {
    pub project_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactRunsQuery {
    pub limit: Option<i64>,
}

#[derive(Debug, Clone)]
struct ProjectLearningConfig {
    enabled: bool,
    prompt_corrections_enabled: bool,
    auto_apply_max_changes_per_day: i32,
    feedback_threshold: i32,
    feedback_window_days: i32,
    min_confidence: f64,
    rollback_window_hours: i32,
}

#[derive(Debug, Clone)]
struct CompactionCandidate {
    id: Uuid,
    point_id: String,
    project_id: Uuid,
    intent: String,
    hash: String,
    status: String,
    created_at: DateTime<Utc>,
    resolved_at: Option<DateTime<Utc>>,
    retrieved_count: i64,
}

// ── Internal helpers ──

async fn load_setting(db: &PgPool, key: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_chain(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|item| item.trim().to_lowercase())
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>()
}

async fn load_current_chain(db: &PgPool, project_id: Uuid, intent: &str) -> Vec<String> {
    let project_key = format!("project_{}_routing_{}_providers", project_id, intent);
    if let Some(value) = load_setting(db, &project_key).await {
        let parsed = parse_chain(&value);
        if !parsed.is_empty() {
            return parsed;
        }
    }

    let global_key = format!("routing_{}_providers", intent);
    if let Some(value) = load_setting(db, &global_key).await {
        let parsed = parse_chain(&value);
        if !parsed.is_empty() {
            return parsed;
        }
    }

    if let Some(value) = load_setting(db, "provider_hierarchy").await {
        let parsed = parse_chain(&value);
        if !parsed.is_empty() {
            return parsed;
        }
    }

    vec![
        "anthropic".to_string(),
        "openai".to_string(),
        "google".to_string(),
    ]
}

fn move_provider_to_end(chain: &[String], provider: &str) -> Vec<String> {
    let mut next = chain
        .iter()
        .filter(|item| item.as_str() != provider)
        .cloned()
        .collect::<Vec<_>>();
    if chain.iter().any(|item| item == provider) {
        next.push(provider.to_string());
    }
    if next.is_empty() {
        chain.to_vec()
    } else {
        next
    }
}

async fn load_or_create_learning_config(
    db: &PgPool,
    project_id: Uuid,
) -> Result<ProjectLearningConfig, ApiError> {
    sqlx::query(
        r#"
        INSERT INTO project_learning_config (
            project_id, enabled, prompt_corrections_enabled, auto_apply_max_changes_per_day,
            feedback_threshold, feedback_window_days, min_confidence, rollback_window_hours,
            created_at, updated_at
        )
        VALUES ($1, TRUE, TRUE, 2, 5, 7, 0.65, 24, NOW(), NOW())
        ON CONFLICT (project_id) DO NOTHING
        "#,
    )
    .bind(project_id)
    .execute(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let row = sqlx::query(
        r#"
        SELECT
            enabled,
            prompt_corrections_enabled,
            auto_apply_max_changes_per_day,
            feedback_threshold,
            feedback_window_days,
            min_confidence::TEXT AS min_confidence,
            rollback_window_hours
        FROM project_learning_config
        WHERE project_id = $1
        "#,
    )
    .bind(project_id)
    .fetch_one(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let min_confidence_raw: String = row
        .try_get("min_confidence")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let min_confidence = min_confidence_raw.parse::<f64>().unwrap_or(0.65);

    Ok(ProjectLearningConfig {
        enabled: row.try_get("enabled").unwrap_or(true),
        prompt_corrections_enabled: row.try_get("prompt_corrections_enabled").unwrap_or(true),
        auto_apply_max_changes_per_day: row.try_get("auto_apply_max_changes_per_day").unwrap_or(2),
        feedback_threshold: row.try_get("feedback_threshold").unwrap_or(5),
        feedback_window_days: row.try_get("feedback_window_days").unwrap_or(7),
        min_confidence,
        rollback_window_hours: row.try_get("rollback_window_hours").unwrap_or(24),
    })
}

async fn maybe_rollback_learning_regression(
    db: &PgPool,
    project_id: Uuid,
    config: &ProjectLearningConfig,
) -> Result<Option<Value>, ApiError> {
    let row = sqlx::query(
        r#"
        SELECT
            l.id AS log_id,
            l.intent,
            l.applied_at,
            s.previous_chain,
            s.baseline_error_count
        FROM learning_decisions_log l
        JOIN learning_policy_snapshots s ON s.id = l.snapshot_id
        WHERE l.project_id = $1
          AND l.action = 'routing_update'
          AND l.status = 'applied'
          AND l.rolled_back_at IS NULL
        ORDER BY l.applied_at DESC
        LIMIT 1
        "#,
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(row) = row else {
        return Ok(None);
    };

    let log_id: Uuid = row
        .try_get("log_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let intent: String = row
        .try_get("intent")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let applied_at: DateTime<Utc> = row
        .try_get("applied_at")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let previous_chain: String = row
        .try_get("previous_chain")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let baseline_error_count: i64 = row
        .try_get("baseline_error_count")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let elapsed_hours = (Utc::now() - applied_at).num_hours();
    if elapsed_hours > i64::from(config.rollback_window_hours) {
        return Ok(None);
    }

    let current_errors = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)
        FROM ai_response_feedback
        WHERE project_id = $1
          AND intent = $2
          AND created_at >= $3
        "#,
    )
    .bind(project_id)
    .bind(&intent)
    .bind(applied_at)
    .fetch_one(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if current_errors <= baseline_error_count + 2 {
        return Ok(None);
    }

    let project_key = format!("project_{}_routing_{}_providers", project_id, intent);
    sqlx::query(
        r#"
        INSERT INTO settings (key, value, category, description, is_secret, updated_at)
        VALUES ($1, $2, 'learning', 'Override routing chain per progetto', FALSE, NOW())
        ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()
        "#,
    )
    .bind(&project_key)
    .bind(&previous_chain)
    .execute(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    sqlx::query(
        r#"
        UPDATE learning_decisions_log
        SET status = 'rolled_back',
            rolled_back_at = NOW(),
            details = details || jsonb_build_object('rollbackReason', 'kpi_regression')
        WHERE id = $1
        "#,
    )
    .bind(log_id)
    .execute(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Some(json!({
        "logId": log_id.to_string(),
        "intent": intent,
        "reason": "kpi_regression",
    })))
}

pub(crate) async fn apply_project_learning(
    db: &PgPool,
    project_id: Uuid,
    triggered_by: Uuid,
    intent_hint: Option<&str>,
    force: bool,
) -> Result<Value, ApiError> {
    let config = load_or_create_learning_config(db, project_id).await?;
    if !config.enabled {
        return Ok(json!({ "status": "disabled" }));
    }

    let intent = intent_hint
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "chat".to_string());

    let feedback_count = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)
        FROM ai_response_feedback
        WHERE project_id = $1
          AND intent = $2
          AND created_at >= NOW() - (($3::TEXT || ' days')::INTERVAL)
        "#,
    )
    .bind(project_id)
    .bind(&intent)
    .bind(config.feedback_window_days)
    .fetch_one(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if !force && feedback_count < i64::from(config.feedback_threshold) {
        return Ok(json!({
            "status": "below_threshold",
            "feedbackCount": feedback_count,
            "threshold": config.feedback_threshold
        }));
    }

    let todays_changes = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)
        FROM learning_decisions_log
        WHERE project_id = $1
          AND action = 'routing_update'
          AND applied_at >= NOW() - INTERVAL '1 day'
        "#,
    )
    .bind(project_id)
    .fetch_one(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if !force && todays_changes >= i64::from(config.auto_apply_max_changes_per_day) {
        return Ok(json!({
            "status": "daily_limit_reached",
            "maxChangesPerDay": config.auto_apply_max_changes_per_day
        }));
    }

    let provider_row = sqlx::query(
        r#"
        SELECT provider, COUNT(*) AS total
        FROM ai_response_feedback
        WHERE project_id = $1
          AND intent = $2
          AND provider IS NOT NULL
          AND created_at >= NOW() - (($3::TEXT || ' days')::INTERVAL)
        GROUP BY provider
        ORDER BY total DESC
        LIMIT 1
        "#,
    )
    .bind(project_id)
    .bind(&intent)
    .bind(config.feedback_window_days)
    .fetch_optional(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(provider_row) = provider_row else {
        return Ok(json!({
            "status": "no_provider_signal",
            "feedbackCount": feedback_count
        }));
    };

    let provider: String = provider_row
        .try_get("provider")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let provider_feedback_count: i64 = provider_row.try_get("total").unwrap_or(0);
    let confidence =
        (provider_feedback_count as f64 / feedback_count.max(1) as f64).max(config.min_confidence);

    if !force && confidence < config.min_confidence {
        return Ok(json!({
            "status": "low_confidence",
            "confidence": confidence,
            "minimum": config.min_confidence
        }));
    }

    let current_chain = load_current_chain(db, project_id, &intent).await;
    let next_chain = move_provider_to_end(&current_chain, &provider);
    if current_chain == next_chain {
        return Ok(json!({
            "status": "no_change",
            "provider": provider,
            "chain": current_chain
        }));
    }

    let snapshot_id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO learning_policy_snapshots (
            id, project_id, intent, previous_chain, next_chain, baseline_error_count,
            snapshot_reason, created_by_user_id, created_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, 'auto_apply', $7, NOW())
        "#,
    )
    .bind(snapshot_id)
    .bind(project_id)
    .bind(&intent)
    .bind(current_chain.join(","))
    .bind(next_chain.join(","))
    .bind(feedback_count)
    .bind(triggered_by)
    .execute(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let project_key = format!("project_{}_routing_{}_providers", project_id, intent);
    sqlx::query(
        r#"
        INSERT INTO settings (key, value, category, description, is_secret, updated_at)
        VALUES ($1, $2, 'learning', 'Override routing chain per progetto', FALSE, NOW())
        ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()
        "#,
    )
    .bind(&project_key)
    .bind(next_chain.join(","))
    .execute(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let decision_id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO learning_decisions_log (
            id, project_id, intent, provider, model, confidence, feedback_count,
            window_days, action, status, snapshot_id, details, created_at, applied_at
        )
        VALUES (
            $1, $2, $3, $4, NULL, $5, $6, $7, 'routing_update',
            'applied', $8, $9, NOW(), NOW()
        )
        "#,
    )
    .bind(decision_id)
    .bind(project_id)
    .bind(&intent)
    .bind(&provider)
    .bind(confidence)
    .bind(feedback_count as i32)
    .bind(config.feedback_window_days)
    .bind(snapshot_id)
    .bind(json!({
        "currentChain": current_chain,
        "nextChain": next_chain,
        "providerFeedbackCount": provider_feedback_count
    }))
    .execute(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let rollback = maybe_rollback_learning_regression(db, project_id, &config).await?;

    Ok(json!({
        "status": "applied",
        "decisionId": decision_id.to_string(),
        "snapshotId": snapshot_id.to_string(),
        "intent": intent,
        "provider": provider,
        "confidence": confidence,
        "currentChain": current_chain,
        "nextChain": next_chain,
        "rollback": rollback,
    }))
}

async fn run_vector_compaction(
    db: &PgPool,
    project_id: Option<Uuid>,
    trigger_type: &str,
    requested_by: Option<Uuid>,
) -> Result<Value, ApiError> {
    let run_id = Uuid::new_v4();
    let before_count = vector_memory::count_prompt_correction_points(db, project_id)
        .await
        .unwrap_or(0);

    sqlx::query(
        r#"
        INSERT INTO vector_compaction_runs (
            id, project_id, trigger_type, status, before_count, requested_by, started_at
        )
        VALUES ($1, $2, $3, 'started', $4, $5, NOW())
        "#,
    )
    .bind(run_id)
    .bind(project_id)
    .bind(trigger_type)
    .bind(before_count)
    .bind(requested_by)
    .execute(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let compaction_result = async {
        let rows = sqlx::query(
            r#"
            SELECT
                id,
                qdrant_point_id,
                project_id,
                COALESCE(intent, 'chat') AS intent,
                normalized_hint_hash,
                status,
                created_at,
                resolved_at,
                retrieved_count
            FROM prompt_corrections
            WHERE active = TRUE
              AND deleted_at IS NULL
              AND ($1::UUID IS NULL OR project_id = $1)
            ORDER BY project_id, COALESCE(intent, 'chat'), normalized_hint_hash, created_at DESC
            "#,
        )
        .bind(project_id)
        .fetch_all(db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        let candidates = rows
            .iter()
            .filter_map(|row| {
                Some(CompactionCandidate {
                    id: row.try_get("id").ok()?,
                    point_id: row.try_get("qdrant_point_id").ok()?,
                    project_id: row.try_get("project_id").ok()?,
                    intent: row.try_get("intent").ok()?,
                    hash: row.try_get("normalized_hint_hash").ok()?,
                    status: row.try_get("status").ok()?,
                    created_at: row.try_get("created_at").ok()?,
                    resolved_at: row.try_get("resolved_at").ok(),
                    retrieved_count: row.try_get("retrieved_count").unwrap_or(0),
                })
            })
            .collect::<Vec<_>>();

        let now = Utc::now();
        let mut prune_map: HashMap<Uuid, &'static str> = HashMap::new();
        let mut seen_keys: HashSet<(Uuid, String, String)> = HashSet::new();

        for item in &candidates {
            if item.status.eq_ignore_ascii_case("rejected") {
                prune_map.insert(item.id, "rejected");
                continue;
            }

            if item.status.eq_ignore_ascii_case("resolved") {
                if let Some(resolved_at) = item.resolved_at {
                    if (now - resolved_at).num_days() >= 30 {
                        prune_map.insert(item.id, "resolved_ttl");
                        continue;
                    }
                }
            }

            if item.retrieved_count == 0 && (now - item.created_at).num_days() >= 90 {
                prune_map.insert(item.id, "unused_ttl");
                continue;
            }

            let dedup_key = (item.project_id, item.intent.clone(), item.hash.clone());
            if seen_keys.contains(&dedup_key) {
                prune_map.insert(item.id, "semantic_dedup");
                continue;
            }
            seen_keys.insert(dedup_key);
        }

        let mut point_ids_to_delete = Vec::new();
        let mut deleted_count = 0_i64;
        let mut dedup_count = 0_i64;
        for item in &candidates {
            if let Some(reason) = prune_map.get(&item.id) {
                sqlx::query(
                    r#"
                    UPDATE prompt_corrections
                    SET active = FALSE,
                        status = 'compacted',
                        deleted_at = NOW(),
                        updated_at = NOW(),
                        metadata = metadata || jsonb_build_object('compactionReason', $2)
                    WHERE id = $1
                      AND deleted_at IS NULL
                    "#,
                )
                .bind(item.id)
                .bind(*reason)
                .execute(db)
                .await
                .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

                point_ids_to_delete.push(item.point_id.clone());
                deleted_count += 1;
                if *reason == "semantic_dedup" {
                    dedup_count += 1;
                }
            }
        }

        let qdrant_deleted_count =
            vector_memory::delete_prompt_correction_points(db, &point_ids_to_delete)
                .await
                .unwrap_or(0) as i64;
        let after_count = vector_memory::count_prompt_correction_points(db, project_id)
            .await
            .unwrap_or(0);

        Ok::<Value, ApiError>(json!({
            "runId": run_id.to_string(),
            "beforeCount": before_count,
            "afterCount": after_count,
            "deletedCount": deleted_count,
            "dedupCount": dedup_count,
            "qdrantDeletedCount": qdrant_deleted_count,
        }))
    }
    .await;

    match compaction_result {
        Ok(summary) => {
            sqlx::query(
                r#"
                UPDATE vector_compaction_runs
                SET status = 'completed',
                    after_count = $2,
                    dedup_count = $3,
                    deleted_count = $4,
                    qdrant_deleted_count = $5,
                    details = $6,
                    finished_at = NOW()
                WHERE id = $1
                "#,
            )
            .bind(run_id)
            .bind(summary["afterCount"].as_i64().unwrap_or(0))
            .bind(summary["dedupCount"].as_i64().unwrap_or(0))
            .bind(summary["deletedCount"].as_i64().unwrap_or(0))
            .bind(summary["qdrantDeletedCount"].as_i64().unwrap_or(0))
            .bind(&summary)
            .execute(db)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            Ok(summary)
        }
        Err(error) => {
            let details = json!({ "error": error.1["error"] });
            let _ = sqlx::query(
                r#"
                UPDATE vector_compaction_runs
                SET status = 'error',
                    details = $2,
                    finished_at = NOW()
                WHERE id = $1
                "#,
            )
            .bind(run_id)
            .bind(details)
            .execute(db)
            .await;
            Err(error)
        }
    }
}

pub(crate) async fn dedup_on_write(
    db: &PgPool,
    project_id: Uuid,
    intent: &str,
    normalized_hint_hash: &str,
    keep_id: Uuid,
) -> Result<i64, ApiError> {
    let rows = sqlx::query(
        r#"
        SELECT id, qdrant_point_id
        FROM prompt_corrections
        WHERE project_id = $1
          AND COALESCE(intent, 'chat') = $2
          AND normalized_hint_hash = $3
          AND active = TRUE
          AND deleted_at IS NULL
        ORDER BY created_at DESC
        "#,
    )
    .bind(project_id)
    .bind(intent)
    .bind(normalized_hint_hash)
    .fetch_all(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut ids_to_prune = Vec::<Uuid>::new();
    let mut points_to_prune = Vec::<String>::new();
    for row in rows {
        let id: Uuid = row
            .try_get("id")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if id == keep_id {
            continue;
        }
        let point_id: String = row
            .try_get("qdrant_point_id")
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        ids_to_prune.push(id);
        points_to_prune.push(point_id);
    }

    if ids_to_prune.is_empty() {
        return Ok(0);
    }

    sqlx::query(
        r#"
        UPDATE prompt_corrections
        SET active = FALSE,
            status = 'deduplicated',
            deleted_at = NOW(),
            updated_at = NOW(),
            metadata = metadata || jsonb_build_object('dedupKeepId', $2::TEXT)
        WHERE id = ANY($1)
        "#,
    )
    .bind(&ids_to_prune)
    .bind(keep_id)
    .execute(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let _ = vector_memory::delete_prompt_correction_points(db, &points_to_prune).await;

    let _ = sqlx::query(
        r#"
        INSERT INTO vector_compaction_runs (
            id, project_id, trigger_type, status, before_count, after_count, dedup_count,
            deleted_count, qdrant_deleted_count, details, started_at, finished_at
        )
        VALUES (
            gen_random_uuid(), $1, 'on_write', 'completed', 0, 0, $2, $2, $3,
            jsonb_build_object('keepId', $4::TEXT, 'intent', $5), NOW(), NOW()
        )
        "#,
    )
    .bind(project_id)
    .bind(ids_to_prune.len() as i64)
    .bind(points_to_prune.len() as i64)
    .bind(keep_id)
    .bind(intent)
    .execute(db)
    .await;

    Ok(ids_to_prune.len() as i64)
}

// ── Public admin handlers ──

pub async fn admin_list_feedback_errors(State(state): State<AppState>) -> ApiResult {
    let rows = sqlx::query(
        r#"
        SELECT
            f.id,
            f.project_id,
            f.session_id,
            f.message_id,
            f.user_id,
            f.intent,
            f.provider,
            f.model,
            f.error_comment,
            f.status,
            f.review_note,
            f.created_at,
            u.email AS user_email,
            pc.correction_text,
            pc.metadata AS correction_metadata,
            pc.retrieved_count
        FROM ai_response_feedback f
        LEFT JOIN users u ON u.id = f.user_id
        LEFT JOIN prompt_corrections pc ON pc.feedback_id = f.id
        ORDER BY f.created_at DESC
        LIMIT 200
        "#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let feedbacks = rows
        .iter()
        .filter_map(|row| {
            let id: Uuid = row.try_get("id").ok()?;
            let project_id: Uuid = row.try_get("project_id").ok()?;
            let session_id: Uuid = row.try_get("session_id").ok()?;
            let message_id: Uuid = row.try_get("message_id").ok()?;
            let user_id: Uuid = row.try_get("user_id").ok()?;
            let created_at: DateTime<Utc> = row.try_get("created_at").ok()?;
            let corr_meta: Option<Value> = row.try_get("correction_metadata").ok().flatten();
            let user_question = corr_meta.as_ref()
                .and_then(|m| m.get("userQuestionPreview"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let ai_response_preview = corr_meta.as_ref()
                .and_then(|m| m.get("aiResponsePreview"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            Some(json!({
                "id": id.to_string(),
                "projectId": project_id.to_string(),
                "sessionId": session_id.to_string(),
                "messageId": message_id.to_string(),
                "userId": user_id.to_string(),
                "userEmail": row.try_get::<Option<String>, _>("user_email").ok().flatten(),
                "intent": row.try_get::<Option<String>, _>("intent").ok().flatten(),
                "provider": row.try_get::<Option<String>, _>("provider").ok().flatten(),
                "model": row.try_get::<Option<String>, _>("model").ok().flatten(),
                "comment": row.try_get::<String, _>("error_comment").ok().unwrap_or_default(),
                "status": row.try_get::<String, _>("status").ok().unwrap_or_else(|| "open".to_string()),
                "reviewNote": row.try_get::<Option<String>, _>("review_note").ok().flatten(),
                "createdAt": created_at.to_rfc3339(),
                "correctionText": row.try_get::<Option<String>, _>("correction_text").ok().flatten(),
                "userQuestionPreview": user_question,
                "aiResponsePreview": ai_response_preview,
                "retrievedCount": row.try_get::<Option<i32>, _>("retrieved_count").ok().flatten().unwrap_or(0),
            }))
        })
        .collect::<Vec<_>>();

    Ok(Json(json!({ "feedback": feedbacks })))
}

pub async fn admin_review_feedback(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(feedback_id): AxumPath<String>,
    Json(body): Json<ReviewFeedbackRequest>,
) -> ApiResult {
    let reviewer_id = parse_user_id(&claims)?;
    let feedback_id = Uuid::parse_str(&feedback_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Feedback id non valido"))?;
    let status = body.status.trim().to_lowercase();
    if !matches!(
        status.as_str(),
        "open" | "reviewed" | "resolved" | "rejected"
    ) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Status non valido (open/reviewed/resolved/rejected)",
        ));
    }

    let row = sqlx::query(
        r#"
        UPDATE ai_response_feedback
        SET status = $2,
            review_note = $3,
            reviewed_by = $4,
            reviewed_at = NOW(),
            updated_at = NOW()
        WHERE id = $1
        RETURNING id, project_id
        "#,
    )
    .bind(feedback_id)
    .bind(&status)
    .bind(body.review_note.as_deref().unwrap_or(""))
    .bind(reviewer_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(row) = row else {
        return Err(api_error(StatusCode::NOT_FOUND, "Feedback non trovato"));
    };

    let project_id: Uuid = row
        .try_get("project_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if status == "resolved" || status == "rejected" {
        let _ = sqlx::query(
            r#"
            UPDATE prompt_corrections
            SET status = $2,
                resolved_at = CASE WHEN $2 = 'resolved' THEN NOW() ELSE resolved_at END,
                active = CASE WHEN $2 = 'rejected' THEN FALSE ELSE active END,
                updated_at = NOW()
            WHERE feedback_id = $1
            "#,
        )
        .bind(feedback_id)
        .bind(&status)
        .execute(&state.db)
        .await;

        if status == "rejected" {
            let points = sqlx::query_scalar::<_, String>(
                r#"
                SELECT qdrant_point_id
                FROM prompt_corrections
                WHERE feedback_id = $1
                "#,
            )
            .bind(feedback_id)
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();
            let _ = vector_memory::delete_prompt_correction_points(&state.db, &points).await;
        }
    }

    Ok(Json(json!({
        "ok": true,
        "feedbackId": feedback_id.to_string(),
        "status": status,
        "projectId": project_id.to_string(),
    })))
}

pub async fn admin_retrain_project_routing(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
    Json(body): Json<RetrainProjectRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = parse_project_id(&project_id)?;
    let decision =
        apply_project_learning(&state.db, project_id, user_id, body.intent.as_deref(), true)
            .await?;

    Ok(Json(json!({
        "ok": true,
        "projectId": project_id.to_string(),
        "decision": decision
    })))
}

pub async fn admin_get_project_learning_config(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
) -> ApiResult {
    let project_id = parse_project_id(&project_id)?;
    let config = load_or_create_learning_config(&state.db, project_id).await?;
    Ok(Json(json!({
        "projectId": project_id.to_string(),
        "config": {
            "enabled": config.enabled,
            "promptCorrectionsEnabled": config.prompt_corrections_enabled,
            "autoApplyMaxChangesPerDay": config.auto_apply_max_changes_per_day,
            "feedbackThreshold": config.feedback_threshold,
            "feedbackWindowDays": config.feedback_window_days,
            "minConfidence": config.min_confidence,
            "rollbackWindowHours": config.rollback_window_hours,
        }
    })))
}

pub async fn admin_update_project_learning_config(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
    Json(body): Json<UpdateProjectLearningConfigRequest>,
) -> ApiResult {
    let project_id = parse_project_id(&project_id)?;
    let current = load_or_create_learning_config(&state.db, project_id).await?;
    let next = ProjectLearningConfig {
        enabled: body.enabled.unwrap_or(current.enabled),
        prompt_corrections_enabled: body
            .prompt_corrections_enabled
            .unwrap_or(current.prompt_corrections_enabled),
        auto_apply_max_changes_per_day: body
            .auto_apply_max_changes_per_day
            .unwrap_or(current.auto_apply_max_changes_per_day),
        feedback_threshold: body
            .feedback_threshold
            .unwrap_or(current.feedback_threshold),
        feedback_window_days: body
            .feedback_window_days
            .unwrap_or(current.feedback_window_days),
        min_confidence: body.min_confidence.unwrap_or(current.min_confidence),
        rollback_window_hours: body
            .rollback_window_hours
            .unwrap_or(current.rollback_window_hours),
    };

    sqlx::query(
        r#"
        UPDATE project_learning_config
        SET enabled = $2,
            prompt_corrections_enabled = $3,
            auto_apply_max_changes_per_day = $4,
            feedback_threshold = $5,
            feedback_window_days = $6,
            min_confidence = $7,
            rollback_window_hours = $8,
            updated_at = NOW()
        WHERE project_id = $1
        "#,
    )
    .bind(project_id)
    .bind(next.enabled)
    .bind(next.prompt_corrections_enabled)
    .bind(next.auto_apply_max_changes_per_day)
    .bind(next.feedback_threshold)
    .bind(next.feedback_window_days)
    .bind(next.min_confidence)
    .bind(next.rollback_window_hours)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({
        "ok": true,
        "projectId": project_id.to_string(),
        "config": {
            "enabled": next.enabled,
            "promptCorrectionsEnabled": next.prompt_corrections_enabled,
            "autoApplyMaxChangesPerDay": next.auto_apply_max_changes_per_day,
            "feedbackThreshold": next.feedback_threshold,
            "feedbackWindowDays": next.feedback_window_days,
            "minConfidence": next.min_confidence,
            "rollbackWindowHours": next.rollback_window_hours,
        }
    })))
}

pub async fn admin_run_vector_compaction(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<CompactVectorRequest>,
) -> ApiResult {
    // Guard: se Qdrant e' down, ritorna errore chiaro invece di timeout
    if !state
        .dependency_status
        .qdrant
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Err(api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Qdrant non disponibile — compaction non avviabile".to_string(),
        ));
    }
    let user_id = parse_user_id(&claims)?;
    let project_id = body
        .project_id
        .as_deref()
        .map(parse_project_id)
        .transpose()?;
    let summary = run_vector_compaction(&state.db, project_id, "manual", Some(user_id)).await?;
    Ok(Json(json!({ "ok": true, "summary": summary })))
}

pub async fn admin_list_vector_compaction_runs(
    State(state): State<AppState>,
    Query(query): Query<CompactRunsQuery>,
) -> ApiResult {
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let rows = sqlx::query(
        r#"
        SELECT
            id,
            project_id,
            trigger_type,
            status,
            before_count,
            after_count,
            dedup_count,
            deleted_count,
            qdrant_deleted_count,
            details,
            requested_by,
            started_at,
            finished_at
        FROM vector_compaction_runs
        ORDER BY started_at DESC
        LIMIT $1
        "#,
    )
    .bind(limit)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let runs = rows
        .iter()
        .filter_map(|row| {
            let id: Uuid = row.try_get("id").ok()?;
            let started_at: DateTime<Utc> = row.try_get("started_at").ok()?;
            let finished_at: Option<DateTime<Utc>> = row.try_get("finished_at").ok();
            Some(json!({
                "id": id.to_string(),
                "projectId": row.try_get::<Option<Uuid>, _>("project_id").ok().flatten().map(|value| value.to_string()),
                "triggerType": row.try_get::<String, _>("trigger_type").ok().unwrap_or_default(),
                "status": row.try_get::<String, _>("status").ok().unwrap_or_default(),
                "beforeCount": row.try_get::<i64, _>("before_count").ok().unwrap_or(0),
                "afterCount": row.try_get::<i64, _>("after_count").ok().unwrap_or(0),
                "dedupCount": row.try_get::<i64, _>("dedup_count").ok().unwrap_or(0),
                "deletedCount": row.try_get::<i64, _>("deleted_count").ok().unwrap_or(0),
                "qdrantDeletedCount": row.try_get::<i64, _>("qdrant_deleted_count").ok().unwrap_or(0),
                "details": row.try_get::<Value, _>("details").ok().unwrap_or_else(|| json!({})),
                "requestedBy": row.try_get::<Option<Uuid>, _>("requested_by").ok().flatten().map(|value| value.to_string()),
                "startedAt": started_at.to_rfc3339(),
                "finishedAt": finished_at.map(|value| value.to_rfc3339()),
            }))
        })
        .collect::<Vec<_>>();

    Ok(Json(json!({ "runs": runs })))
}

/// Parsa il cron expression letto da DB e restituisce (hour, minute).
/// Formato atteso: "MIN HOUR * * *" (es. "0 2 * * *").
/// Fallback: (2, 0) ovvero 02:00.
fn parse_compaction_cron(cron: &str) -> (u32, u32) {
    let parts: Vec<&str> = cron.split_whitespace().collect();
    if parts.len() >= 2 {
        let minute = parts[0].parse::<u32>().unwrap_or(0).min(59);
        let hour = parts[1].parse::<u32>().unwrap_or(2).min(23);
        return (hour, minute);
    }
    (2, 0)
}

pub fn spawn_vector_compaction_scheduler(state: AppState) {
    tokio::spawn(async move {
        loop {
            // Legge il cron dalla setting DB ad ogni iterazione (permette hot-reload).
            let cron_str = sqlx::query_scalar::<_, String>(
                "SELECT value FROM settings WHERE key = 'vector_compaction_schedule_cron'",
            )
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "0 2 * * *".to_string());

            let (hour, minute) = parse_compaction_cron(&cron_str);

            let now = Local::now();
            let today = now.date_naive();
            let mut next = today.and_hms_opt(hour, minute, 0).unwrap_or_else(|| {
                today
                    .and_hms_milli_opt(hour, minute, 0, 0)
                    .expect("valid time")
            });

            if now.naive_local() >= next {
                next += chrono::Duration::days(1);
            }

            let wait = (next - now.naive_local())
                .to_std()
                .unwrap_or_else(|_| Duration::from_secs(60 * 60));

            tracing::info!(
                "vector_compaction_scheduler: prossima run alle {:02}:{:02} (cron='{}', wait={:.0}min)",
                hour, minute, cron_str,
                wait.as_secs_f64() / 60.0
            );
            tokio::time::sleep(wait).await;

            // Guard: se Qdrant e' down, skip compaction (il watchdog la marcherebbe comunque stale)
            if !state
                .dependency_status
                .qdrant
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                tracing::info!("vector_compaction_scheduler: skip — Qdrant non disponibile");
                continue;
            }
            let _ = run_vector_compaction(&state.db, None, "scheduled", None).await;
        }
    });
}

// ─── Admin: Prompt Corrections ────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct CreatePromptCorrectionBody {
    /// Testo della correzione (es. "Quando l'utente chiede X, intende Y").
    pub text: String,
    /// Intent associato (opzionale, es. "code_edit").
    pub intent: Option<String>,
    /// Progetto di appartenenza (opzionale — se assente usa un progetto globale).
    pub project_id: Option<Uuid>,
}

/// POST /api/admin/prompt-corrections
/// Inserisce una correzione prompt in PostgreSQL e in Qdrant.
pub async fn admin_create_prompt_correction(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    Json(body): Json<CreatePromptCorrectionBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let text = body.text.trim().to_string();
    if text.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "il campo 'text' non può essere vuoto"})),
        ));
    }

    // Usa il progetto fornito o il primo disponibile come scope globale.
    let project_id: Uuid = match body.project_id {
        Some(id) => id,
        None => {
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM projects ORDER BY created_at ASC LIMIT 1")
                .fetch_optional(&state.db)
                .await
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error": format!("DB error: {e}")})),
                    )
                })?
                .ok_or_else(|| {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": "nessun progetto trovato — specificare project_id"})),
                    )
                })?
        }
    };

    // Genera embedding tramite l'orchestrator.
    let embedding = state.orchestrator.embed_text(&text).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("embedding fallito: {e}")})),
        )
    })?;

    // Hash per deduplicazione.
    let hash = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(text.as_bytes());
        format!("{:x}", h.finalize())[..16].to_string()
    };

    let point_id = Uuid::new_v4().to_string();
    let intent = body.intent.clone().unwrap_or_else(|| "general".to_string());

    let payload = json!({
        "correction_id": point_id,
        "text": text,
        "intent": intent,
        "project_id": project_id.to_string(),
    });

    vector_memory::upsert_prompt_correction_point(&state.db, &point_id, &embedding, payload)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("upsert Qdrant fallito: {e}")})),
            )
        })?;

    // Persiste in PostgreSQL.
    let correction_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO prompt_corrections
            (project_id, intent, correction_text, normalized_hint_hash, qdrant_point_id, active, status)
        VALUES ($1, $2, $3, $4, $5, true, 'open')
        ON CONFLICT (qdrant_point_id) DO UPDATE
            SET correction_text = EXCLUDED.correction_text,
                updated_at = NOW()
        RETURNING id
        "#,
    )
    .bind(project_id)
    .bind(&intent)
    .bind(&text)
    .bind(&hash)
    .bind(&point_id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("DB insert fallito: {e}")})),
        )
    })?;

    Ok(Json(json!({
        "id": correction_id.to_string(),
        "qdrant_point_id": point_id,
        "intent": intent,
        "project_id": project_id.to_string(),
        "text": text,
        "active": true,
    })))
}

/// GET /api/admin/prompt-corrections
/// Lista le correzioni attive.
pub async fn admin_list_prompt_corrections(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let rows = sqlx::query!(
        r#"
        SELECT id, project_id, intent, correction_text, qdrant_point_id,
               active, status, retrieved_count, created_at
        FROM prompt_corrections
        WHERE deleted_at IS NULL
        ORDER BY created_at DESC
        LIMIT 100
        "#
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("DB error: {e}")})),
        )
    })?;

    let corrections: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id.to_string(),
                "projectId": r.project_id.to_string(),
                "intent": r.intent,
                "text": r.correction_text,
                "qdrantPointId": r.qdrant_point_id,
                "active": r.active,
                "status": r.status,
                "retrievedCount": r.retrieved_count,
                "createdAt": r.created_at.to_rfc3339(),
            })
        })
        .collect();

    Ok(Json(
        json!({"corrections": corrections, "total": corrections.len()}),
    ))
}

/// DELETE /api/admin/prompt-corrections/:id
/// Soft-delete di una correzione (marca deleted_at e disabilita).
pub async fn admin_delete_prompt_correction(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let affected = sqlx::query(
        "UPDATE prompt_corrections SET deleted_at = NOW(), active = false, updated_at = NOW() WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .execute(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("DB error: {e}")})),
        )
    })?
    .rows_affected();

    if affected == 0 {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "correzione non trovata"})),
        ));
    }

    Ok(Json(json!({"deleted": true, "id": id.to_string()})))
}
