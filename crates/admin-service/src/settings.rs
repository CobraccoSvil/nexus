use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::AppState;

// Tipi DTO: punto unico in nexus_types::settings_dto (regola L / ADR 0026, S8).
pub use nexus_types::settings_dto::{
    BulkUpdateRequest, CreateDirectoryRequest, FsBrowseQuery, Setting,
    UpdateSettingRequest,
};

// FS browse: punto unico in nexus_types::fs_browse (regola L / ADR 0026).
use nexus_types::fs_browse::{list_directories, list_root_candidates};
// Tipi e helper API: punto unico in nexus_types (regola L / ADR 0026, cluster E6).
// Prima `ApiError`/`ApiResult`/`api_error`/`validate_directory_name` erano
// ri-implementati identici qui e in crates/mcp-core/src/settings.rs.
use nexus_types::{api_error, validate_directory_name_api as validate_directory_name, ApiResult};

async fn ensure_required_settings(state: &AppState) {
    // Default statici: migrazione 0325 (regola G/H). Parte dinamica
    // (projects_base_root): punto unico in nexus-types (prima duplicata qui e
    // in mcp-core).
    nexus_types::ensure_projects_base_root(&state.db).await;
}

pub async fn browse_directories(Query(query): Query<FsBrowseQuery>) -> ApiResult {
    let roots = list_root_candidates();
    let target = if let Some(path) = query.path {
        let trimmed = path.trim().to_string();
        if trimmed.is_empty() { roots[0].clone() }
        else { PathBuf::from(trimmed).canonicalize().map_err(|_| api_error(StatusCode::BAD_REQUEST, "Percorso non valido"))? }
    } else { roots[0].clone() };

    if !target.is_dir() { return Err(api_error(StatusCode::BAD_REQUEST, "Non e' una directory")); }

    let target_str = target.to_string_lossy().to_string();
    let parent_path = target.parent().and_then(|p| {
        let ps = p.to_string_lossy().to_string();
        if ps == target_str { None } else { Some(ps) }
    });

    Ok(Json(json!({
        "roots": roots.iter().map(|r| r.to_string_lossy().to_string()).collect::<Vec<_>>(),
        "currentPath": target_str,
        "parentPath": parent_path,
        "directories": list_directories(&target),
    })))
}

pub async fn create_directory(Json(body): Json<CreateDirectoryRequest>) -> ApiResult {
    let parent = PathBuf::from(body.parent_path.trim())
        .canonicalize()
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Percorso parent non valido"))?;
    if !parent.is_dir() { return Err(api_error(StatusCode::BAD_REQUEST, "Il parent non e' una directory")); }

    let dir_name = validate_directory_name(&body.name)?;
    let target = parent.join(dir_name);
    if target.exists() { return Err(api_error(StatusCode::CONFLICT, "Directory gia' esistente")); }

    std::fs::create_dir(&target).map_err(|e| {
        let status = match e.kind() {
            std::io::ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
            std::io::ErrorKind::AlreadyExists => StatusCode::CONFLICT,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        api_error(status, e.to_string())
    })?;

    Ok(Json(json!({ "ok": true, "path": target.to_string_lossy().to_string() })))
}

pub async fn list_settings(State(state): State<AppState>) -> Json<Value> {
    ensure_required_settings(&state).await;

    // Fix S87: prima .unwrap_or_default() mostrava "0 settings" su DB down.
    let settings = match sqlx::query_as::<_, Setting>(
        "SELECT key, value, category, description, is_secret, updated_at FROM settings ORDER BY category, key",
    ).fetch_all(&state.db).await {
        Ok(rows) => rows,
        Err(e) => { tracing::warn!("list_settings: SELECT fallito: {}", e); Vec::new() }
    };

    let masked: Vec<Value> = settings.into_iter().map(|s| {
        let display_value = if s.is_secret && !s.value.is_empty() {
            format!("{}...****", &s.value[..4.min(s.value.len())])
        } else if s.is_secret { String::new() } else { s.value.clone() };
        json!({ "key": s.key, "value": display_value, "category": s.category, "description": s.description, "is_secret": s.is_secret, "updated_at": s.updated_at, "has_value": !s.value.is_empty() })
    }).collect();

    Json(json!({ "settings": masked }))
}

pub async fn list_by_category(
    State(state): State<AppState>,
    Path(category): Path<String>,
) -> Json<Value> {
    ensure_required_settings(&state).await;

    // Fix S87: vedi list_settings.
    let settings = match sqlx::query_as::<_, Setting>(
        "SELECT key, value, category, description, is_secret, updated_at FROM settings WHERE category = $1 ORDER BY key",
    ).bind(&category).fetch_all(&state.db).await {
        Ok(rows) => rows,
        Err(e) => { tracing::warn!("list_by_category({}): SELECT fallito: {}", category, e); Vec::new() }
    };

    let masked: Vec<Value> = settings.into_iter().map(|s| {
        let display_value = if s.is_secret && !s.value.is_empty() {
            format!("{}...****", &s.value[..4.min(s.value.len())])
        } else if s.is_secret { String::new() } else { s.value.clone() };
        json!({ "key": s.key, "value": display_value, "category": s.category, "description": s.description, "is_secret": s.is_secret, "updated_at": s.updated_at, "has_value": !s.value.is_empty() })
    }).collect();

    Json(json!({ "settings": masked }))
}

pub async fn update_setting(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(body): Json<UpdateSettingRequest>,
) -> Json<Value> {
    let result = sqlx::query("UPDATE settings SET value = $1, updated_at = NOW() WHERE key = $2")
        .bind(&body.value).bind(&key).execute(&state.db).await;

    match result {
        Ok(r) if r.rows_affected() > 0 => Json(json!({ "status": "ok", "key": key })),
        Ok(_) => {
            let _ = sqlx::query("INSERT INTO settings (key, value, category, description, is_secret) VALUES ($1, $2, 'custom', '', FALSE)")
                .bind(&key).bind(&body.value).execute(&state.db).await;
            Json(json!({ "status": "created", "key": key }))
        }
        Err(e) => Json(json!({ "status": "error", "error": e.to_string() })),
    }
}

pub async fn bulk_update(
    State(state): State<AppState>,
    Json(body): Json<BulkUpdateRequest>,
) -> Json<Value> {
    ensure_required_settings(&state).await;

    let mut updated = 0;
    let mut errors = Vec::new();

    for entry in &body.settings {
        match sqlx::query(
            "INSERT INTO settings (key, value, category, description, is_secret, updated_at) VALUES ($1, $2, 'custom', '', FALSE, NOW()) ON CONFLICT (key) DO UPDATE SET value = $2, updated_at = NOW()",
        ).bind(&entry.key).bind(&entry.value).execute(&state.db).await {
            Ok(_) => updated += 1,
            Err(e) => errors.push(format!("{}: {}", entry.key, e)),
        }
    }

    let has_api_key = body.settings.iter().any(|e| e.key.ends_with("_api_key"));
    if has_api_key && errors.is_empty() {
        let brain_url = std::env::var("NEURAL_CORE_REST_URL").unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());
        tokio::spawn(async move {
            let client = nexus_http::NexusClient::with_timeout(5).inner().clone();
            match client.post(format!("{brain_url}/reload-settings")).json(&json!({"mcp_core_url": "http://localhost:4000"})).send().await {
                Ok(r) => tracing::info!("Brain reload-settings: {}", r.status()),
                Err(e) => tracing::warn!("Brain reload-settings failed: {e}"),
            }
        });
    }

    Json(json!({ "status": if errors.is_empty() { "ok" } else { "partial" }, "updated": updated, "errors": errors }))
}

pub async fn get_raw_value(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Json<Value> {
    // Fix S87: prima ingoiava silenziosamente errore DB.
    let value = match sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
        .bind(&key).fetch_optional(&state.db).await {
        Ok(opt) => opt.unwrap_or_default(),
        Err(e) => { tracing::warn!("get_raw_value({}): SELECT fallito: {}", key, e); String::new() }
    };
    Json(json!({ "key": key, "value": value }))
}
