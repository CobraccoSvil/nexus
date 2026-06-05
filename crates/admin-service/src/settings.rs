use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::AppState;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Setting {
    pub key: String,
    pub value: String,
    pub category: String,
    pub description: String,
    pub is_secret: bool,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingRequest {
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct BulkUpdateRequest {
    pub settings: Vec<BulkSettingEntry>,
}

#[derive(Debug, Deserialize)]
pub struct BulkSettingEntry {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct FsBrowseQuery {
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDirectoryRequest {
    pub parent_path: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowseDirectoryNode {
    name: String,
    path: String,
    has_children: bool,
}

type ApiError = (StatusCode, Json<Value>);
type ApiResult = Result<Json<Value>, ApiError>;

fn api_error(status: StatusCode, message: impl Into<String>) -> ApiError {
    (status, Json(json!({ "error": message.into() })))
}

fn list_root_candidates() -> Vec<PathBuf> {
    if cfg!(windows) {
        let mut roots = Vec::new();
        for letter in 'A'..='Z' {
            let candidate = PathBuf::from(format!("{letter}:\\"));
            if candidate.exists() { roots.push(candidate); }
        }
        if roots.is_empty() { roots.push(PathBuf::from("C:\\")); }
        roots
    } else {
        vec![PathBuf::from("/")]
    }
}

fn list_directories(target: &std::path::Path) -> Vec<BrowseDirectoryNode> {
    let mut directories = std::fs::read_dir(target)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(|entry| entry.ok()))
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = entry.metadata().ok()?;
            if !metadata.is_dir() { return None; }
            let name = entry.file_name().to_string_lossy().to_string();
            let has_children = std::fs::read_dir(&path)
                .ok()
                .map(|children| children.filter_map(|c| c.ok()).any(|c| c.metadata().map(|m| m.is_dir()).unwrap_or(false)))
                .unwrap_or(false);
            Some(BrowseDirectoryNode { name, path: path.to_string_lossy().to_string(), has_children })
        })
        .collect::<Vec<_>>();
    directories.sort_by(|a, b| a.name.cmp(&b.name));
    directories
}

fn validate_directory_name(name: &str) -> Result<&str, ApiError> {
    let trimmed = name.trim();
    if trimmed.is_empty() { return Err(api_error(StatusCode::BAD_REQUEST, "Il nome della directory e' obbligatorio")); }
    if trimmed == "." || trimmed == ".." { return Err(api_error(StatusCode::BAD_REQUEST, "Il nome della directory non e' valido")); }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains('\0') {
        return Err(api_error(StatusCode::BAD_REQUEST, "Il nome della directory non puo' contenere separatori"));
    }
    Ok(trimmed)
}

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

    let settings = sqlx::query_as::<_, Setting>(
        "SELECT key, value, category, description, is_secret, updated_at FROM settings ORDER BY category, key",
    ).fetch_all(&state.db).await.unwrap_or_default();

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

    let settings = sqlx::query_as::<_, Setting>(
        "SELECT key, value, category, description, is_secret, updated_at FROM settings WHERE category = $1 ORDER BY key",
    ).bind(&category).fetch_all(&state.db).await.unwrap_or_default();

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
    let value = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
        .bind(&key).fetch_optional(&state.db).await.ok().flatten().unwrap_or_default();
    Json(json!({ "key": key, "value": value }))
}
