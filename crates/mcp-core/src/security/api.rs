//! Endpoint REST per i pannelli frontend di sicurezza e quote.
//!
//! - `GET /api/projects/:id/security/audit` — lista eventi di audit per il progetto
//! - `GET /api/projects/:id/security/quota` — stato quota attuale vs utilizzo

use axum::{
    extract::{Extension, Path as AxumPath, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::auth::Claims;
use crate::projects::{api_error, parse_user_id};
use crate::AppState;

type ApiError = (StatusCode, Json<Value>);
type ApiResult = Result<Json<Value>, ApiError>;

#[derive(Deserialize)]
pub struct AuditQuery {
    /// Limite righe (default 50, max 200)
    pub limit: Option<i64>,
    /// Offset per paginazione
    pub offset: Option<i64>,
    /// Filtro outcome: "allowed", "blocked", "killed", o tutti se assente
    pub outcome: Option<String>,
    /// Filtro azione (es. "port_allocate", "command_blocked")
    pub action: Option<String>,
}

/// GET /api/projects/:id/security/audit
/// Ritorna le ultime voci di audit per il progetto.
pub async fn get_project_audit(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<AuditQuery>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    let limit = q.limit.unwrap_or(50).min(200).max(1);
    let offset = q.offset.unwrap_or(0).max(0);

    // Costruisci query dinamica con filtri opzionali
    let mut sql = String::from(
        "SELECT id, ts, actor, action, resource_kind, resource_id, outcome, details \
         FROM nexus_resource_audit WHERE project_id = $1",
    );
    let mut param_idx = 2;

    if q.outcome.is_some() {
        sql.push_str(&format!(" AND outcome = ${}", param_idx));
        param_idx += 1;
    }
    if q.action.is_some() {
        sql.push_str(&format!(" AND action = ${}", param_idx));
        // param_idx += 1; // non servono altri parametri dopo
    }

    sql.push_str(" ORDER BY ts DESC");
    sql.push_str(&format!(" LIMIT {} OFFSET {}", limit, offset));

    let mut query = sqlx::query(&sql).bind(project_id);
    if let Some(ref outcome) = q.outcome {
        query = query.bind(outcome);
    }
    if let Some(ref action) = q.action {
        query = query.bind(action);
    }

    let rows = query
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<i64, _>("id").unwrap_or(0),
                "ts": r.try_get::<chrono::DateTime<chrono::Utc>, _>("ts")
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_default(),
                "actor": r.try_get::<String, _>("actor").unwrap_or_default(),
                "action": r.try_get::<String, _>("action").unwrap_or_default(),
                "resource_kind": r.try_get::<String, _>("resource_kind").unwrap_or_default(),
                "resource_id": r.try_get::<Option<String>, _>("resource_id").unwrap_or(None),
                "outcome": r.try_get::<String, _>("outcome").unwrap_or_default(),
                "details": r.try_get::<Value, _>("details").unwrap_or(json!({})),
            })
        })
        .collect();

    // Conta totale per paginazione
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM nexus_resource_audit WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    Ok(Json(json!({
        "items": items,
        "total": total,
        "limit": limit,
        "offset": offset,
    })))
}

/// GET /api/projects/:id/security/quota
/// Ritorna lo stato attuale delle quote: limiti e utilizzo corrente.
pub async fn get_project_quota(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    let quota = super::quotas::load_quota(&state.db, project_id).await;

    // Conteggi attuali
    let ports_used: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM nexus_port_allocations WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    let containers_used: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM agent_processes \
         WHERE project_id = $1 AND status IN ('running', 'starting') AND sandboxed = true",
    )
    .bind(project_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    // Conteggio eventi audit recenti (ultime 24h) per statistiche rapide
    let audit_24h: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM nexus_resource_audit \
         WHERE project_id = $1 AND ts > NOW() - INTERVAL '24 hours'",
    )
    .bind(project_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    let blocked_24h: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM nexus_resource_audit \
         WHERE project_id = $1 AND ts > NOW() - INTERVAL '24 hours' AND outcome = 'blocked'",
    )
    .bind(project_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    Ok(Json(json!({
        "quota": {
            "max_ports": quota.max_ports,
            "max_memory_mb": quota.max_memory_mb,
            "max_disk_mb": quota.max_disk_mb,
            "max_containers": quota.max_containers,
            "max_db_pool_size": quota.max_db_pool_size,
        },
        "usage": {
            "ports": ports_used,
            "containers": containers_used,
        },
        "audit_stats": {
            "events_24h": audit_24h,
            "blocked_24h": blocked_24h,
        },
    })))
}
