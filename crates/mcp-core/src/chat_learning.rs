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

/// Errore HTTP 500 uniforme per gli handler che ritornano `ApiError`.
/// Punto unico di costruzione (CLAUDE.md regola L): evita di ripetere ovunque
/// `api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())`.
fn internal_err(e: impl std::fmt::Display) -> ApiError {
    api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// UPSERT della chiave `project_{id}_routing_{intent}_providers` in `settings`.
/// Punto unico (CLAUDE.md regola L): stesso INSERT ... ON CONFLICT usato sia in
/// fase di apply che di rollback della routing chain. Comportamento invariato.
async fn upsert_project_routing_setting(
    db: &PgPool,
    project_key: &str,
    chain_value: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        INSERT INTO settings (key, value, category, description, is_secret, updated_at)
        VALUES ($1, $2, 'learning', 'Override routing chain per progetto', FALSE, NOW())
        ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()
        "#,
    )
    .bind(project_key)
    .bind(chain_value)
    .execute(db)
    .await
    .map_err(internal_err)?;
    // Questa scrittura ha una query propria (upsert con categoria) e quindi non
    // passa dal punto unico che invalida: senza, la lettura continuerebbe a
    // servire il vecchio valore per tutto il TTL della cache dei settings.
    nexus_auth::invalidate_setting_cache(db, project_key);
    Ok(())
}

/// Estrae la config di learning da una riga letta da `project_learning_config`.
fn config_from_row(row: &sqlx::postgres::PgRow) -> Result<ProjectLearningConfig, ApiError> {
    let min_confidence_raw: String = row.try_get("min_confidence").map_err(internal_err)?;
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

/// Fonde una richiesta di update con la config corrente: ogni campo assente
/// nella richiesta conserva il valore attuale.
fn merge_config(
    current: &ProjectLearningConfig,
    body: &UpdateProjectLearningConfigRequest,
) -> ProjectLearningConfig {
    ProjectLearningConfig {
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
    }
}

/// Serializza la config di learning nel blocco JSON esposto dagli handler admin.
/// Punto unico (CLAUDE.md regola L): stesso payload per get e update.
fn config_json(config: &ProjectLearningConfig) -> Value {
    json!({
        "enabled": config.enabled,
        "promptCorrectionsEnabled": config.prompt_corrections_enabled,
        "autoApplyMaxChangesPerDay": config.auto_apply_max_changes_per_day,
        "feedbackThreshold": config.feedback_threshold,
        "feedbackWindowDays": config.feedback_window_days,
        "minConfidence": config.min_confidence,
        "rollbackWindowHours": config.rollback_window_hours,
    })
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
    .map_err(internal_err)?;

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
    .map_err(internal_err)?;

    config_from_row(&row)
}

/// Dati della piu' recente decisione di routing applicata e non ancora
/// ri-annullata, candidata a rollback. Estratto per tenere
/// `maybe_rollback_learning_regression` sotto soglia (comportamento invariato).
struct RollbackCandidate {
    log_id: Uuid,
    intent: String,
    applied_at: DateTime<Utc>,
    previous_chain: String,
    baseline_error_count: i64,
}

/// Legge la decisione di routing piu' recente candidata a rollback (se esiste).
async fn load_rollback_candidate(
    db: &PgPool,
    project_id: Uuid,
) -> Result<Option<RollbackCandidate>, ApiError> {
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
    .map_err(internal_err)?;

    let Some(row) = row else {
        return Ok(None);
    };

    Ok(Some(RollbackCandidate {
        log_id: row.try_get("log_id").map_err(internal_err)?,
        intent: row.try_get("intent").map_err(internal_err)?,
        applied_at: row.try_get("applied_at").map_err(internal_err)?,
        previous_chain: row.try_get("previous_chain").map_err(internal_err)?,
        baseline_error_count: row.try_get("baseline_error_count").map_err(internal_err)?,
    }))
}

/// Conta i feedback registrati per il progetto/intent a partire da `since`.
/// separazione DB: ai_response_feedback vive nel pool del progetto (flag ON).
async fn count_feedback_since(
    db: &PgPool,
    project_id: Uuid,
    intent: &str,
    since: DateTime<Utc>,
) -> Result<i64, ApiError> {
    let feedback_pool = crate::project_db_routes::project_data_pool_from(db, project_id).await;
    sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)
        FROM ai_response_feedback
        WHERE project_id = $1
          AND intent = $2
          AND created_at >= $3
        "#,
    )
    .bind(project_id)
    .bind(intent)
    .bind(since)
    .fetch_one(&feedback_pool)
    .await
    .map_err(internal_err)
}

async fn maybe_rollback_learning_regression(
    db: &PgPool,
    project_id: Uuid,
    config: &ProjectLearningConfig,
) -> Result<Option<Value>, ApiError> {
    let Some(candidate) = load_rollback_candidate(db, project_id).await? else {
        return Ok(None);
    };

    let elapsed_hours = (Utc::now() - candidate.applied_at).num_hours();
    if elapsed_hours > i64::from(config.rollback_window_hours) {
        return Ok(None);
    }

    let current_errors =
        count_feedback_since(db, project_id, &candidate.intent, candidate.applied_at).await?;
    if current_errors <= candidate.baseline_error_count + 2 {
        return Ok(None);
    }

    let project_key = format!(
        "project_{}_routing_{}_providers",
        project_id, candidate.intent
    );
    upsert_project_routing_setting(db, &project_key, &candidate.previous_chain).await?;

    sqlx::query(
        r#"
        UPDATE learning_decisions_log
        SET status = 'rolled_back',
            rolled_back_at = NOW(),
            details = details || jsonb_build_object('rollbackReason', 'kpi_regression')
        WHERE id = $1
        "#,
    )
    .bind(candidate.log_id)
    .execute(db)
    .await
    .map_err(internal_err)?;

    Ok(Some(json!({
        "logId": candidate.log_id.to_string(),
        "intent": candidate.intent,
        "reason": "kpi_regression",
    })))
}

/// Conta i feedback nel window in giorni per progetto/intent (pool del progetto).
async fn count_feedback_window(
    feedback_pool: &PgPool,
    project_id: Uuid,
    intent: &str,
    window_days: i32,
) -> Result<i64, ApiError> {
    sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)
        FROM ai_response_feedback
        WHERE project_id = $1
          AND intent = $2
          AND created_at >= NOW() - (($3::TEXT || ' days')::INTERVAL)
        "#,
    )
    .bind(project_id)
    .bind(intent)
    .bind(window_days)
    .fetch_one(feedback_pool)
    .await
    .map_err(internal_err)
}

/// Conta gli update di routing gia' applicati al progetto nelle ultime 24h (meta).
async fn count_todays_routing_changes(db: &PgPool, project_id: Uuid) -> Result<i64, ApiError> {
    sqlx::query_scalar::<_, i64>(
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
    .map_err(internal_err)
}

/// Provider con piu' feedback nel window (nome + conteggio), se esiste segnale.
async fn top_provider_in_window(
    feedback_pool: &PgPool,
    project_id: Uuid,
    intent: &str,
    window_days: i32,
) -> Result<Option<(String, i64)>, ApiError> {
    let row = sqlx::query(
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
    .bind(intent)
    .bind(window_days)
    .fetch_optional(feedback_pool)
    .await
    .map_err(internal_err)?;

    let Some(row) = row else {
        return Ok(None);
    };
    let provider: String = row.try_get("provider").map_err(internal_err)?;
    let total: i64 = row.try_get("total").unwrap_or(0);
    Ok(Some((provider, total)))
}

/// Parametri di persistenza di un update di routing (snapshot + settings + log).
/// Estratto per tenere `apply_project_learning` sotto soglia (comportamento
/// invariato): raccoglie i valori gia' calcolati dal chiamante.
struct RoutingUpdate<'a> {
    project_id: Uuid,
    triggered_by: Uuid,
    intent: &'a str,
    provider: &'a str,
    confidence: f64,
    feedback_count: i64,
    window_days: i32,
    current_chain: &'a [String],
    next_chain: &'a [String],
    provider_feedback_count: i64,
}

/// INSERT dello snapshot di policy (chain precedente/nuova + baseline errori).
async fn insert_policy_snapshot(
    db: &PgPool,
    snapshot_id: Uuid,
    update: &RoutingUpdate<'_>,
) -> Result<(), ApiError> {
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
    .bind(update.project_id)
    .bind(update.intent)
    .bind(update.current_chain.join(","))
    .bind(update.next_chain.join(","))
    .bind(update.feedback_count)
    .bind(update.triggered_by)
    .execute(db)
    .await
    .map_err(internal_err)?;
    Ok(())
}

/// INSERT della decisione applicata nel log (stato 'applied', collegata allo snapshot).
async fn insert_decision_log(
    db: &PgPool,
    decision_id: Uuid,
    snapshot_id: Uuid,
    update: &RoutingUpdate<'_>,
) -> Result<(), ApiError> {
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
    .bind(update.project_id)
    .bind(update.intent)
    .bind(update.provider)
    .bind(update.confidence)
    .bind(update.feedback_count as i32)
    .bind(update.window_days)
    .bind(snapshot_id)
    .bind(json!({
        "currentChain": update.current_chain,
        "nextChain": update.next_chain,
        "providerFeedbackCount": update.provider_feedback_count
    }))
    .execute(db)
    .await
    .map_err(internal_err)?;
    Ok(())
}

/// Persiste snapshot, override settings e decision log di un update di routing.
/// Ritorna `(snapshot_id, decision_id)`. Ordine/query identici all'originale.
async fn persist_routing_update(
    db: &PgPool,
    update: &RoutingUpdate<'_>,
) -> Result<(Uuid, Uuid), ApiError> {
    let snapshot_id = Uuid::new_v4();
    insert_policy_snapshot(db, snapshot_id, update).await?;

    let project_key = format!(
        "project_{}_routing_{}_providers",
        update.project_id, update.intent
    );
    upsert_project_routing_setting(db, &project_key, &update.next_chain.join(",")).await?;

    let decision_id = Uuid::new_v4();
    insert_decision_log(db, decision_id, snapshot_id, update).await?;

    Ok((snapshot_id, decision_id))
}

/// Esito della fase di gate di `apply_project_learning`: o si ferma con un JSON
/// diagnostico (soglie non superate), oppure prosegue con il provider scelto.
enum LearningGate {
    Halt(Value),
    Proceed {
        provider: String,
        provider_feedback_count: i64,
        confidence: f64,
        feedback_count: i64,
    },
}

/// Controlla le soglie di volume (numero feedback e limite giornaliero di
/// update). Ritorna `Some(json)` se un guard blocca l'apply, `None` per proseguire.
async fn precheck_volume_thresholds(
    db: &PgPool,
    project_id: Uuid,
    config: &ProjectLearningConfig,
    force: bool,
    feedback_count: i64,
) -> Result<Option<Value>, ApiError> {
    if !force && feedback_count < i64::from(config.feedback_threshold) {
        return Ok(Some(json!({
            "status": "below_threshold",
            "feedbackCount": feedback_count,
            "threshold": config.feedback_threshold
        })));
    }

    let todays_changes = count_todays_routing_changes(db, project_id).await?;
    if !force && todays_changes >= i64::from(config.auto_apply_max_changes_per_day) {
        return Ok(Some(json!({
            "status": "daily_limit_reached",
            "maxChangesPerDay": config.auto_apply_max_changes_per_day
        })));
    }
    Ok(None)
}

/// Applica i guard di soglia (feedback, limite giornaliero, segnale provider,
/// confidenza). Estratto per tenere `apply_project_learning` sotto soglia:
/// stessa sequenza di controlli e stessi JSON di early-return dell'originale.
async fn evaluate_learning_gate(
    db: &PgPool,
    feedback_pool: &PgPool,
    project_id: Uuid,
    intent: &str,
    config: &ProjectLearningConfig,
    force: bool,
) -> Result<LearningGate, ApiError> {
    let feedback_count = count_feedback_window(
        feedback_pool,
        project_id,
        intent,
        config.feedback_window_days,
    )
    .await?;

    if let Some(halt) =
        precheck_volume_thresholds(db, project_id, config, force, feedback_count).await?
    {
        return Ok(LearningGate::Halt(halt));
    }

    // separazione DB: ai_response_feedback nel pool del progetto (riuso feedback_pool)
    let Some((provider, provider_feedback_count)) = top_provider_in_window(
        feedback_pool,
        project_id,
        intent,
        config.feedback_window_days,
    )
    .await?
    else {
        return Ok(LearningGate::Halt(json!({
            "status": "no_provider_signal",
            "feedbackCount": feedback_count
        })));
    };

    let confidence =
        (provider_feedback_count as f64 / feedback_count.max(1) as f64).max(config.min_confidence);
    if !force && confidence < config.min_confidence {
        return Ok(LearningGate::Halt(json!({
            "status": "low_confidence",
            "confidence": confidence,
            "minimum": config.min_confidence
        })));
    }

    Ok(LearningGate::Proceed {
        provider,
        provider_feedback_count,
        confidence,
        feedback_count,
    })
}

/// Fase di applicazione: calcola la nuova chain, persiste l'update, tenta il
/// rollback e costruisce il JSON di risposta. Estratto dal chiamante gate-side
/// per tenere `apply_project_learning` sotto soglia (comportamento invariato).
async fn commit_learning_update(
    db: &PgPool,
    project_id: Uuid,
    triggered_by: Uuid,
    intent: &str,
    config: &ProjectLearningConfig,
    gate: (String, i64, f64, i64),
) -> Result<Value, ApiError> {
    let (provider, provider_feedback_count, confidence, feedback_count) = gate;
    let current_chain = load_current_chain(db, project_id, intent).await;
    let next_chain = move_provider_to_end(&current_chain, &provider);
    if current_chain == next_chain {
        return Ok(json!({
            "status": "no_change",
            "provider": provider,
            "chain": current_chain
        }));
    }

    let (snapshot_id, decision_id) = persist_routing_update(
        db,
        &RoutingUpdate {
            project_id,
            triggered_by,
            intent,
            provider: &provider,
            confidence,
            feedback_count,
            window_days: config.feedback_window_days,
            current_chain: &current_chain,
            next_chain: &next_chain,
            provider_feedback_count,
        },
    )
    .await?;
    let rollback = maybe_rollback_learning_regression(db, project_id, config).await?;

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

    // separazione DB: ai_response_feedback vive nel pool del progetto (flag ON);
    // pool riusato per le query feedback successive nello stesso scope
    let feedback_pool = crate::project_db_routes::project_data_pool_from(db, project_id).await;
    let gate = match evaluate_learning_gate(db, &feedback_pool, project_id, &intent, &config, force)
        .await?
    {
        LearningGate::Halt(value) => return Ok(value),
        LearningGate::Proceed {
            provider,
            provider_feedback_count,
            confidence,
            feedback_count,
        } => (
            provider,
            provider_feedback_count,
            confidence,
            feedback_count,
        ),
    };

    commit_learning_update(db, project_id, triggered_by, &intent, &config, gate).await
}

/// Registra l'avvio di una run di compaction (stato 'started') sul meta-DB.
async fn insert_compaction_run_start(
    db: &PgPool,
    run_id: Uuid,
    project_id: Option<Uuid>,
    trigger_type: &str,
    before_count: i64,
    requested_by: Option<Uuid>,
) -> Result<(), ApiError> {
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
    .map_err(internal_err)?;
    Ok(())
}

/// Carica le correzioni attive candidate a compaction dal pool `cpool`.
async fn load_compaction_candidates(
    cpool: &PgPool,
    project_id: Option<Uuid>,
) -> Result<Vec<CompactionCandidate>, ApiError> {
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
    .fetch_all(cpool)
    .await
    .map_err(internal_err)?;

    Ok(rows
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
        .collect::<Vec<_>>())
}

/// Logica PURA (nessun IO) che, dati i candidati, decide quali potare e con
/// quale motivo. Estratta per testabilita' e per tenere la funzione principale
/// sotto soglia. Comportamento identico all'inline originale.
fn plan_compaction_pruning(candidates: &[CompactionCandidate]) -> HashMap<Uuid, &'static str> {
    let now = Utc::now();
    let mut prune_map: HashMap<Uuid, &'static str> = HashMap::new();
    let mut seen_keys: HashSet<(Uuid, String, String)> = HashSet::new();

    for item in candidates {
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
    prune_map
}

/// Applica il piano di pruning: marca 'compacted' le righe candidate su `cpool`
/// e ritorna (point_ids Qdrant da eliminare, deleted_count, dedup_count).
async fn execute_compaction_pruning(
    cpool: &PgPool,
    candidates: &[CompactionCandidate],
    prune_map: &HashMap<Uuid, &'static str>,
) -> Result<(Vec<String>, i64, i64), ApiError> {
    let mut point_ids_to_delete = Vec::new();
    let mut deleted_count = 0_i64;
    let mut dedup_count = 0_i64;
    for item in candidates {
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
            .execute(cpool)
            .await
            .map_err(internal_err)?;

            point_ids_to_delete.push(item.point_id.clone());
            deleted_count += 1;
            if *reason == "semantic_dedup" {
                dedup_count += 1;
            }
        }
    }
    Ok((point_ids_to_delete, deleted_count, dedup_count))
}

/// Corpo della compaction: carica candidati, pianifica ed esegue il pruning,
/// elimina i punti Qdrant e costruisce il JSON di riepilogo. NB: Qdrant e i
/// conteggi usano `db` (meta), il pruning SQL usa `cpool` (pool progetto): la
/// separazione dei due pool e' identica all'inline originale.
async fn compute_compaction(
    db: &PgPool,
    cpool: &PgPool,
    project_id: Option<Uuid>,
    run_id: Uuid,
    before_count: i64,
) -> Result<Value, ApiError> {
    let candidates = load_compaction_candidates(cpool, project_id).await?;
    let prune_map = plan_compaction_pruning(&candidates);
    let (point_ids_to_delete, deleted_count, dedup_count) =
        execute_compaction_pruning(cpool, &candidates, &prune_map).await?;

    let qdrant_deleted_count =
        vector_memory::delete_prompt_correction_points(db, &point_ids_to_delete)
            .await
            .unwrap_or(0) as i64;
    let after_count = vector_memory::count_prompt_correction_points(db, project_id)
        .await
        .unwrap_or(0);

    Ok(json!({
        "runId": run_id.to_string(),
        "beforeCount": before_count,
        "afterCount": after_count,
        "deletedCount": deleted_count,
        "dedupCount": dedup_count,
        "qdrantDeletedCount": qdrant_deleted_count,
    }))
}

/// Chiude una run di compaction: stato 'completed' col summary, oppure 'error'
/// col dettaglio, propagando lo stesso `Result` ricevuto. Ordine/query identici.
async fn finalize_compaction_run(
    db: &PgPool,
    run_id: Uuid,
    result: Result<Value, ApiError>,
) -> Result<Value, ApiError> {
    match result {
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
            .map_err(internal_err)?;
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

    insert_compaction_run_start(
        db,
        run_id,
        project_id,
        trigger_type,
        before_count,
        requested_by,
    )
    .await?;

    // Separazione DB: prompt_corrections vive nel pool del progetto quando
    // project_id e' noto; vector_compaction_runs (sopra/sotto) NON e' migrata e
    // resta sul meta. NB: con project_id=None (compaction GLOBALE) al flag ON
    // questo processa solo il meta (vuoto) -> la compaction globale va triggerata
    // per-progetto (project_id valorizzato). A flag OFF -> meta, invariato.
    let cpool = match project_id {
        Some(pid) => crate::project_db_routes::project_data_pool_from(db, pid).await,
        None => db.clone(),
    };

    let compaction_result = compute_compaction(db, &cpool, project_id, run_id, before_count).await;
    finalize_compaction_run(db, run_id, compaction_result).await
}

/// Carica i "fratelli" attivi con stesso hash (candidati alla deduplica),
/// partizionandoli in (ids, qdrant_point_ids) ed escludendo `keep_id`.
async fn load_dedup_siblings(
    corrections_pool: &PgPool,
    project_id: Uuid,
    intent: &str,
    normalized_hint_hash: &str,
    keep_id: Uuid,
) -> Result<(Vec<Uuid>, Vec<String>), ApiError> {
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
    .fetch_all(corrections_pool)
    .await
    .map_err(internal_err)?;

    let mut ids_to_prune = Vec::<Uuid>::new();
    let mut points_to_prune = Vec::<String>::new();
    for row in rows {
        let id: Uuid = row.try_get("id").map_err(internal_err)?;
        if id == keep_id {
            continue;
        }
        let point_id: String = row.try_get("qdrant_point_id").map_err(internal_err)?;
        ids_to_prune.push(id);
        points_to_prune.push(point_id);
    }
    Ok((ids_to_prune, points_to_prune))
}

/// Registra su `vector_compaction_runs` (meta) l'esito della dedup on-write.
/// L'errore e' volutamente ignorato (best-effort telemetria), come nell'originale.
async fn log_on_write_dedup_run(
    db: &PgPool,
    project_id: Uuid,
    intent: &str,
    keep_id: Uuid,
    pruned_ids: i64,
    pruned_points: i64,
) {
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
    .bind(pruned_ids)
    .bind(pruned_points)
    .bind(keep_id)
    .bind(intent)
    .execute(db)
    .await;
}

/// Marca 'deduplicated' le correzioni da potare (id in `ids_to_prune`), tracciando
/// l'id conservato in metadata. UPDATE sul pool del progetto (riuso corrections_pool).
async fn mark_dedup_pruned(
    corrections_pool: &PgPool,
    ids_to_prune: &[Uuid],
    keep_id: Uuid,
) -> Result<(), ApiError> {
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
    .bind(ids_to_prune)
    .bind(keep_id)
    .execute(corrections_pool)
    .await
    .map_err(internal_err)?;
    Ok(())
}

pub(crate) async fn dedup_on_write(
    db: &PgPool,
    project_id: Uuid,
    intent: &str,
    normalized_hint_hash: &str,
    keep_id: Uuid,
) -> Result<i64, ApiError> {
    // separazione DB: prompt_corrections vive nel pool del progetto (flag ON);
    // pool riusato per la UPDATE di dedup nello stesso scope
    let corrections_pool = crate::project_db_routes::project_data_pool_from(db, project_id).await;
    let (ids_to_prune, points_to_prune) = load_dedup_siblings(
        &corrections_pool,
        project_id,
        intent,
        normalized_hint_hash,
        keep_id,
    )
    .await?;

    if ids_to_prune.is_empty() {
        return Ok(0);
    }

    // separazione DB: prompt_corrections nel pool del progetto (riuso corrections_pool)
    mark_dedup_pruned(&corrections_pool, &ids_to_prune, keep_id).await?;
    let _ = vector_memory::delete_prompt_correction_points(db, &points_to_prune).await;

    log_on_write_dedup_run(
        db,
        project_id,
        intent,
        keep_id,
        ids_to_prune.len() as i64,
        points_to_prune.len() as i64,
    )
    .await;

    Ok(ids_to_prune.len() as i64)
}

// ── Public admin handlers ──

/// Aggrega i feedback dai DB-progetto (dedup per id), ordina per data desc e
/// tronca ai primi 200. Estratto da `admin_list_feedback_errors`.
async fn collect_global_feedback_rows(state: &AppState) -> Vec<sqlx::postgres::PgRow> {
    let mut rows: Vec<sqlx::postgres::PgRow> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for pid in crate::project_db_routes::list_all_project_ids(&state.db).await {
        let pool = crate::project_db_routes::project_data_pool_from(&state.db, pid).await;
        let batch = sqlx::query(
            r#"
            SELECT f.id, f.project_id, f.session_id, f.message_id, f.user_id, f.intent,
                   f.provider, f.model, f.error_comment, f.status, f.review_note, f.created_at,
                   pc.correction_text, pc.metadata AS correction_metadata, pc.retrieved_count
            FROM ai_response_feedback f
            LEFT JOIN prompt_corrections pc ON pc.feedback_id = f.id
            ORDER BY f.created_at DESC
            LIMIT 200
            "#,
        )
        .fetch_all(&pool)
        .await
        .unwrap_or_default();
        for r in batch {
            if let Ok(id) = r.try_get::<Uuid, _>("id") {
                if seen.insert(id) {
                    rows.push(r);
                }
            }
        }
    }
    rows.sort_by(|a, b| {
        let bb = b.try_get::<DateTime<Utc>, _>("created_at").ok();
        let aa = a.try_get::<DateTime<Utc>, _>("created_at").ok();
        bb.cmp(&aa)
    });
    rows.truncate(200);
    rows
}

/// Risolve email utente dal meta-DB (`users` non migrata) con un solo SELECT.
async fn resolve_user_emails(
    state: &AppState,
    rows: &[sqlx::postgres::PgRow],
) -> std::collections::HashMap<Uuid, String> {
    let user_ids: Vec<Uuid> = rows
        .iter()
        .filter_map(|r| r.try_get::<Uuid, _>("user_id").ok())
        .collect();
    let mut email_by_user: std::collections::HashMap<Uuid, String> =
        std::collections::HashMap::new();
    if !user_ids.is_empty() {
        if let Ok(urows) = sqlx::query("SELECT id, email FROM users WHERE id = ANY($1)")
            .bind(&user_ids)
            .fetch_all(&state.db)
            .await
        {
            for ur in urows {
                if let (Ok(uid), Ok(email)) = (
                    ur.try_get::<Uuid, _>("id"),
                    ur.try_get::<String, _>("email"),
                ) {
                    email_by_user.insert(uid, email);
                }
            }
        }
    }
    email_by_user
}

/// Serializza una riga di feedback (con correction join) nel JSON esposto.
fn feedback_row_json(
    row: &sqlx::postgres::PgRow,
    email_by_user: &std::collections::HashMap<Uuid, String>,
) -> Option<Value> {
    let id: Uuid = row.try_get("id").ok()?;
    let project_id: Uuid = row.try_get("project_id").ok()?;
    let session_id: Uuid = row.try_get("session_id").ok()?;
    let message_id: Uuid = row.try_get("message_id").ok()?;
    let user_id: Uuid = row.try_get("user_id").ok()?;
    let created_at: DateTime<Utc> = row.try_get("created_at").ok()?;
    let corr_meta: Option<Value> = row.try_get("correction_metadata").ok().flatten();
    let user_question = corr_meta
        .as_ref()
        .and_then(|m| m.get("userQuestionPreview"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let ai_response_preview = corr_meta
        .as_ref()
        .and_then(|m| m.get("aiResponsePreview"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    Some(json!({
        "id": id.to_string(),
        "projectId": project_id.to_string(),
        "sessionId": session_id.to_string(),
        "messageId": message_id.to_string(),
        "userId": user_id.to_string(),
        "userEmail": email_by_user.get(&user_id).cloned(),
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
}

pub async fn admin_list_feedback_errors(State(state): State<AppState>) -> ApiResult {
    // Vista admin GLOBALE: ai_response_feedback + prompt_corrections sono migrate
    // (JOIN valido sul pool del progetto); `users` NON e' migrata (resta su meta)
    // -> split del JOIN. Aggrega iterando i DB-progetto (a flag OFF i pool sono il
    // meta -> dedup per id), poi risolve le email utente dal meta. Top 200 globale.
    let rows = collect_global_feedback_rows(&state).await;
    let email_by_user = resolve_user_emails(&state, &rows).await;
    let feedbacks = rows
        .iter()
        .filter_map(|row| feedback_row_json(row, &email_by_user))
        .collect::<Vec<_>>();

    Ok(Json(json!({ "feedback": feedbacks })))
}

/// Applica lo stato di review al feedback e ritorna il `project_id` (None se il
/// feedback non esiste). UPDATE ... RETURNING sul pool del progetto.
async fn update_feedback_status(
    corrections_pool: &PgPool,
    feedback_id: Uuid,
    status: &str,
    review_note: &str,
    reviewer_id: Uuid,
) -> Result<Option<Uuid>, ApiError> {
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
    .bind(status)
    .bind(review_note)
    .bind(reviewer_id)
    .fetch_optional(corrections_pool)
    .await
    .map_err(internal_err)?;

    match row {
        Some(row) => Ok(Some(row.try_get("project_id").map_err(internal_err)?)),
        None => Ok(None),
    }
}

/// Propaga uno stato terminale (`resolved`/`rejected`) alle correzioni collegate:
/// aggiorna `prompt_corrections` e, su `rejected`, elimina i punti da Qdrant.
/// Best-effort come nell'originale (errori ignorati sui side-effect).
async fn propagate_feedback_resolution(
    state: &AppState,
    corrections_pool: &PgPool,
    feedback_id: Uuid,
    status: &str,
) {
    if status != "resolved" && status != "rejected" {
        return;
    }
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
    .bind(status)
    .execute(corrections_pool)
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
        .fetch_all(corrections_pool)
        .await
        .unwrap_or_default();
        let _ = vector_memory::delete_prompt_correction_points(&state.db, &points).await;
    }
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

    // separazione DB: endpoint keyed solo dal feedback_id. ai_response_feedback e
    // prompt_corrections (stesso progetto) vivono nel DB del progetto -> pool via
    // directory di routing (fallback ricerca), riusato per entrambe.
    let corrections_pool =
        crate::project_db_routes::project_data_pool_by_feedback_from(&state.db, feedback_id).await;
    let Some(project_id) = update_feedback_status(
        &corrections_pool,
        feedback_id,
        &status,
        body.review_note.as_deref().unwrap_or(""),
        reviewer_id,
    )
    .await?
    else {
        return Err(api_error(StatusCode::NOT_FOUND, "Feedback non trovato"));
    };

    propagate_feedback_resolution(&state, &corrections_pool, feedback_id, &status).await;

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
        "config": config_json(&config),
    })))
}

pub async fn admin_update_project_learning_config(
    State(state): State<AppState>,
    AxumPath(project_id): AxumPath<String>,
    Json(body): Json<UpdateProjectLearningConfigRequest>,
) -> ApiResult {
    let project_id = parse_project_id(&project_id)?;
    let current = load_or_create_learning_config(&state.db, project_id).await?;
    let next = merge_config(&current, &body);

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
    .map_err(internal_err)?;

    Ok(Json(json!({
        "ok": true,
        "projectId": project_id.to_string(),
        "config": config_json(&next),
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

/// Serializza una riga di `vector_compaction_runs` nel JSON esposto dall'API.
fn compaction_run_json(row: &sqlx::postgres::PgRow) -> Option<Value> {
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
    .map_err(internal_err)?;

    let runs = rows
        .iter()
        .filter_map(compaction_run_json)
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

/// Legge il cron dalla setting DB (hot-reload ad ogni iterazione) e calcola
/// (hour, minute, cron_str, attesa fino alla prossima esecuzione).
async fn next_compaction_wait(db: &PgPool) -> (u32, u32, String, Duration) {
    let cron_str = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'vector_compaction_schedule_cron'",
    )
    .fetch_optional(db)
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
    (hour, minute, cron_str, wait)
}

pub fn spawn_vector_compaction_scheduler(state: AppState) {
    tokio::spawn(async move {
        loop {
            let (hour, minute, cron_str, wait) = next_compaction_wait(&state.db).await;

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

/// Errore HTTP in formato tuple `(StatusCode, Json<Value>)` per gli handler
/// prompt-corrections (che non usano `ApiError`). Punto unico locale (regola L).
fn json_err(status: StatusCode, message: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": message.to_string() })))
}

/// Risolve il project_id da usare: quello del body o, se assente, il primo
/// progetto per data di creazione (scope globale). Errore 400 se nessuno esiste.
async fn resolve_correction_project_id(
    state: &AppState,
    body_project_id: Option<Uuid>,
) -> Result<Uuid, (StatusCode, Json<Value>)> {
    match body_project_id {
        Some(id) => Ok(id),
        None => {
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM projects ORDER BY created_at ASC LIMIT 1")
                .fetch_optional(&state.db)
                .await
                .map_err(|e| json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}")))?
                .ok_or_else(|| {
                    json_err(
                        StatusCode::BAD_REQUEST,
                        "nessun progetto trovato — specificare project_id",
                    )
                })
        }
    }
}

/// SHA-256 esadecimale troncato a 16 char, usato come hash di deduplicazione.
fn short_hash_16(text: &str) -> String {
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())[..16].to_string()
}

/// INSERT ... ON CONFLICT della correzione sul pool del progetto, ritorna l'id.
async fn insert_prompt_correction_row(
    corrections_pool: &PgPool,
    project_id: Uuid,
    intent: &str,
    text: &str,
    hash: &str,
    point_id: &str,
) -> Result<Uuid, (StatusCode, Json<Value>)> {
    sqlx::query_scalar(
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
    .bind(intent)
    .bind(text)
    .bind(hash)
    .bind(point_id)
    .fetch_one(corrections_pool)
    .await
    .map_err(|e| json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("inserimento nel DB fallito: {e}")))
}

/// Genera l'embedding del testo e ne fa l'upsert come punto Qdrant col payload
/// standard della correzione. Estratto da `admin_create_prompt_correction`.
async fn embed_and_upsert_correction(
    state: &AppState,
    text: &str,
    intent: &str,
    project_id: Uuid,
    point_id: &str,
) -> Result<(), (StatusCode, Json<Value>)> {
    let embedding = state.orchestrator.embed_text(text).await.map_err(|e| {
        json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("embedding fallito: {e}"),
        )
    })?;

    let payload = json!({
        "correction_id": point_id,
        "text": text,
        "intent": intent,
        "project_id": project_id.to_string(),
    });

    vector_memory::upsert_prompt_correction_point(&state.db, point_id, &embedding, payload)
        .await
        .map_err(|e| {
            json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("upsert Qdrant fallito: {e}"),
            )
        })
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
        return Err(json_err(
            StatusCode::BAD_REQUEST,
            "il campo 'text' non può essere vuoto",
        ));
    }

    let project_id = resolve_correction_project_id(&state, body.project_id).await?;
    let hash = short_hash_16(&text);
    let point_id = Uuid::new_v4().to_string();
    let intent = body.intent.clone().unwrap_or_else(|| "general".to_string());

    embed_and_upsert_correction(&state, &text, &intent, project_id, &point_id).await?;

    // Persiste in PostgreSQL.
    // separazione DB: prompt_corrections vive nel pool del progetto (flag ON)
    let corrections_pool =
        crate::project_db_routes::project_data_pool_from(&state.db, project_id).await;
    let correction_id = insert_prompt_correction_row(
        &corrections_pool,
        project_id,
        &intent,
        &text,
        &hash,
        &point_id,
    )
    .await?;

    Ok(Json(json!({
        "id": correction_id.to_string(),
        "qdrant_point_id": point_id,
        "intent": intent,
        "project_id": project_id.to_string(),
        "text": text,
        "active": true,
    })))
}

/// Riga runtime di `prompt_corrections` (query non-macro post-0507: la tabella
/// e' migrata e non esiste piu' nel meta contro cui `sqlx::query!` si prepara).
type CorrRow = (
    Uuid,
    Uuid,
    String,
    String,
    Option<String>,
    bool,
    String,
    i32,
    chrono::DateTime<chrono::Utc>,
);

/// Aggrega le correzioni attive dai DB-progetto (dedup per id), ordina per data
/// desc e tronca alle prime 100. Estratto da `admin_list_prompt_corrections`.
async fn collect_global_corrections(state: &AppState) -> Vec<CorrRow> {
    let mut rows: Vec<CorrRow> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for pid in crate::project_db_routes::list_all_project_ids(&state.db).await {
        let pool = crate::project_db_routes::project_data_pool_from(&state.db, pid).await;
        let batch: Vec<CorrRow> = sqlx::query_as(
            r#"
            SELECT id, project_id, intent, correction_text, qdrant_point_id,
                   active, status, retrieved_count, created_at
            FROM prompt_corrections
            WHERE deleted_at IS NULL
            ORDER BY created_at DESC
            LIMIT 100
            "#,
        )
        .fetch_all(&pool)
        .await
        .unwrap_or_default();
        for r in batch {
            if seen.insert(r.0) {
                rows.push(r);
            }
        }
    }
    rows.sort_by(|a, b| b.8.cmp(&a.8));
    rows.truncate(100);
    rows
}

/// GET /api/admin/prompt-corrections
/// Lista le correzioni attive.
///
/// Vista admin GLOBALE: prompt_corrections e' migrata -> aggrega iterando i
/// DB-progetto. A flag OFF tutti i pool sono il meta (dedup per id evita i
/// duplicati); a flag ON ogni progetto ha le sue righe. Top 100 globale.
pub async fn admin_list_prompt_corrections(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let rows = collect_global_corrections(&state).await;
    let corrections: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.0.to_string(), "projectId": r.1.to_string(),
                "intent": r.2, "text": r.3,
                "qdrantPointId": r.4, "active": r.5,
                "status": r.6, "retrievedCount": r.7,
                "createdAt": r.8.to_rfc3339(),
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
    // separazione DB: endpoint keyed solo dalla correzione -> pool del progetto via
    // directory di routing (fallback ricerca). A flag OFF -> meta-DB.
    let cpool = crate::project_db_routes::project_data_pool_by_correction_from(&state.db, id).await;
    let affected = sqlx::query(
        "UPDATE prompt_corrections SET deleted_at = NOW(), active = false, updated_at = NOW() WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .execute(&cpool)
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
