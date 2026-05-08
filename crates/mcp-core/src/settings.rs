use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use sqlx::PgPool;
use tokio;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

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
            if candidate.exists() {
                roots.push(candidate);
            }
        }
        if roots.is_empty() {
            roots.push(PathBuf::from("C:\\"));
        }
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
            if !metadata.is_dir() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let has_children = std::fs::read_dir(&path)
                .ok()
                .map(|children| {
                    children
                        .filter_map(|child| child.ok())
                        .any(|child| child.metadata().map(|m| m.is_dir()).unwrap_or(false))
                })
                .unwrap_or(false);

            Some(BrowseDirectoryNode {
                name,
                path: path.to_string_lossy().to_string(),
                has_children,
            })
        })
        .collect::<Vec<_>>();

    directories.sort_by(|left, right| left.name.cmp(&right.name));
    directories
}

fn validate_directory_name(name: &str) -> Result<&str, ApiError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il nome della directory e' obbligatorio",
        ));
    }
    if trimmed == "." || trimmed == ".." {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il nome della directory non e' valido",
        ));
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains('\0') {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il nome della directory non puo' contenere separatori di percorso",
        ));
    }
    Ok(trimmed)
}

fn map_create_dir_error(error: std::io::Error) -> ApiError {
    let status = match error.kind() {
        std::io::ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
        std::io::ErrorKind::AlreadyExists => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    api_error(status, error.to_string())
}

async fn ensure_required_settings(state: &super::AppState) {
    // DLP settings: seed nel DB così la UI "Sicurezza & Privacy" ha sempre i toggle disponibili.
    // I valori di default vengono presi da env (NEXUS_DLP_*). Se l'utente ha già impostato qualcosa
    // nel DB (valore non vuoto), non lo sovrascriviamo.
    let dlp_enabled = std::env::var("NEXUS_DLP_ENABLED").unwrap_or_else(|_| "true".to_string());
    let allow_tier2 = std::env::var("NEXUS_ALLOW_CLOUD_TIER2").unwrap_or_else(|_| "true".to_string());
    let allow_tier3 = std::env::var("NEXUS_ALLOW_CLOUD_TIER3").unwrap_or_else(|_| "false".to_string());

    let _ = sqlx::query(
        r#"
        INSERT INTO settings (key, value, category, description, is_secret, updated_at)
        VALUES
          ('dlp_enabled', $1, 'security', 'Abilita/disabilita il Data Loss Prevention (classificazione sensibilità Tier).', FALSE, NOW())
        ON CONFLICT (key) DO UPDATE
        SET value = EXCLUDED.value,
            updated_at = NOW()
        WHERE settings.value IS NULL OR btrim(settings.value) = ''
        "#,
    )
    .bind(&dlp_enabled)
    .execute(&state.db)
    .await;

    let _ = sqlx::query(
        r#"
        INSERT INTO settings (key, value, category, description, is_secret, updated_at)
        VALUES
          ('dlp_allow_cloud_tier2', $1, 'security', 'Se true, consente di inviare Tier 2 (sensibili) verso provider cloud.', FALSE, NOW())
        ON CONFLICT (key) DO UPDATE
        SET value = EXCLUDED.value,
            updated_at = NOW()
        WHERE settings.value IS NULL OR btrim(settings.value) = ''
        "#,
    )
    .bind(&allow_tier2)
    .execute(&state.db)
    .await;

    let _ = sqlx::query(
        r#"
        INSERT INTO settings (key, value, category, description, is_secret, updated_at)
        VALUES
          ('dlp_allow_cloud_tier3', $1, 'security', 'Se true, consente di inviare Tier 3 (critici) verso provider cloud (sconsigliato).', FALSE, NOW())
        ON CONFLICT (key) DO UPDATE
        SET value = EXCLUDED.value,
            updated_at = NOW()
        WHERE settings.value IS NULL OR btrim(settings.value) = ''
        "#,
    )
    .bind(&allow_tier3)
    .execute(&state.db)
    .await;

    let _ = sqlx::query(
        r#"
        INSERT INTO settings (key, value, category, description, is_secret, updated_at)
        VALUES (
            'projects_base_root',
            '',
            'infrastructure',
            'Root assoluta sotto cui e'' consentita la registrazione/navigazione dei progetti',
            FALSE,
            NOW()
        )
        ON CONFLICT (key) DO NOTHING
        "#,
    )
    .execute(&state.db)
    .await;

    let _ = sqlx::query(
        r#"
        INSERT INTO settings (key, value, category, description, is_secret, updated_at)
        VALUES
            ('agent_parallel_enabled', 'false', 'agent', 'Abilita l''esecuzione parallela di piu'' agenti contemporaneamente per accelerare task complessi', FALSE, NOW()),
            ('agent_parallel_max', '3', 'agent', 'Numero massimo di agenti paralleli per sessione (1-5)', FALSE, NOW())
        ON CONFLICT (key) DO NOTHING
        "#,
    )
    .execute(&state.db)
    .await;

    let _ = sqlx::query(
        r#"
        INSERT INTO settings (key, value, category, description, is_secret, updated_at)
        VALUES (
            'network_dns_servers',
            '',
            'infrastructure',
            'Server DNS personalizzati separati da virgola (es. 8.8.8.8,1.1.1.1). Usato dal Neural Core per risolvere i nomi host verso API AI esterne.',
            FALSE,
            NOW()
        )
        ON CONFLICT (key) DO NOTHING
        "#,
    )
    .execute(&state.db)
    .await;

    let _ = sqlx::query(
        r#"
        INSERT INTO settings (key, value, category, description, is_secret, updated_at)
        VALUES (
            'nexus_external_proxy',
            '',
            'infrastructure',
            'Proxy HTTP/HTTPS per le chiamate verso API esterne (es. http://localhost:8002). Usato da tutti i backend Nexus tramite NEXUS_PROXY. Lascia vuoto per connessione diretta.',
            FALSE,
            NOW()
        )
        ON CONFLICT (key) DO NOTHING
        "#,
    )
    .execute(&state.db)
    .await;

    let default_root = std::env::current_dir()
        .map(|cwd| cwd.join("projects"))
        .ok()
        .and_then(|path| {
            if std::fs::create_dir_all(&path).is_ok() {
                path.canonicalize().ok()
            } else {
                None
            }
        })
        .map(|path| path.to_string_lossy().to_string());

    if let Some(root_value) = default_root {
        let _ = sqlx::query(
            r#"
            UPDATE settings
            SET value = $1, updated_at = NOW()
            WHERE key = 'projects_base_root'
              AND (value IS NULL OR btrim(value) = '')
            "#,
        )
        .bind(root_value)
        .execute(&state.db)
        .await;
    }
}

/// GET /api/admin/fs/directories — browse server filesystem (admin only)
pub async fn browse_directories(Query(query): Query<FsBrowseQuery>) -> ApiResult {
    let roots = list_root_candidates();
    let target = if let Some(path) = query.path {
        let trimmed = path.trim().to_string();
        if trimmed.is_empty() {
            roots[0].clone()
        } else {
            PathBuf::from(trimmed)
                .canonicalize()
                .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Percorso directory non valido"))?
        }
    } else {
        roots[0].clone()
    };

    if !target.is_dir() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il percorso selezionato non e' una directory",
        ));
    }

    let target_str = target.to_string_lossy().to_string();
    let parent_path = target.parent().and_then(|parent| {
        let parent_str = parent.to_string_lossy().to_string();
        if parent_str == target_str {
            None
        } else {
            Some(parent_str)
        }
    });

    Ok(Json(json!({
        "roots": roots
            .iter()
            .map(|root| root.to_string_lossy().to_string())
            .collect::<Vec<_>>(),
        "currentPath": target_str,
        "parentPath": parent_path,
        "directories": list_directories(&target),
    })))
}

/// POST /api/admin/fs/directories/create — create directory on server filesystem (admin only)
pub async fn create_directory(Json(body): Json<CreateDirectoryRequest>) -> ApiResult {
    let parent = PathBuf::from(body.parent_path.trim())
        .canonicalize()
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Percorso directory non valido"))?;

    if !parent.is_dir() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il percorso parent non e' una directory",
        ));
    }

    let dir_name = validate_directory_name(&body.name)?;
    let target = parent.join(dir_name);
    if target.exists() {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Esiste gia' una directory con questo nome",
        ));
    }

    std::fs::create_dir(&target).map_err(map_create_dir_error)?;

    Ok(Json(json!({
        "ok": true,
        "path": target.to_string_lossy().to_string(),
    })))
}

/// GET /api/settings — all settings (secrets are masked)
pub async fn list_settings(State(state): State<super::AppState>) -> Json<serde_json::Value> {
    ensure_required_settings(&state).await;

    let settings = sqlx::query_as::<_, Setting>(
        "SELECT key, value, category, description, is_secret, updated_at FROM settings ORDER BY category, key",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let masked: Vec<serde_json::Value> = settings
        .into_iter()
        .map(|s| {
            let display_value = if s.is_secret && !s.value.is_empty() {
                format!("{}...{}", &s.value[..4.min(s.value.len())], "****")
            } else if s.is_secret {
                String::new()
            } else {
                s.value.clone()
            };
            serde_json::json!({
                "key": s.key,
                "value": display_value,
                "category": s.category,
                "description": s.description,
                "is_secret": s.is_secret,
                "updated_at": s.updated_at,
                "has_value": !s.value.is_empty(),
            })
        })
        .collect();

    Json(serde_json::json!({ "settings": masked }))
}

/// GET /api/settings/:category — settings filtered by category
pub async fn list_by_category(
    State(state): State<super::AppState>,
    Path(category): Path<String>,
) -> Json<serde_json::Value> {
    ensure_required_settings(&state).await;

    let settings = sqlx::query_as::<_, Setting>(
        "SELECT key, value, category, description, is_secret, updated_at FROM settings WHERE category = $1 ORDER BY key",
    )
    .bind(&category)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let masked: Vec<serde_json::Value> = settings
        .into_iter()
        .map(|s| {
            let display_value = if s.is_secret && !s.value.is_empty() {
                format!("{}...{}", &s.value[..4.min(s.value.len())], "****")
            } else if s.is_secret {
                String::new()
            } else {
                s.value.clone()
            };
            serde_json::json!({
                "key": s.key,
                "value": display_value,
                "category": s.category,
                "description": s.description,
                "is_secret": s.is_secret,
                "updated_at": s.updated_at,
                "has_value": !s.value.is_empty(),
            })
        })
        .collect();

    Json(serde_json::json!({ "settings": masked }))
}

/// PUT /api/settings/:key — update a single setting
pub async fn update_setting(
    State(state): State<super::AppState>,
    Path(key): Path<String>,
    Json(body): Json<UpdateSettingRequest>,
) -> Json<serde_json::Value> {
    let result = sqlx::query("UPDATE settings SET value = $1, updated_at = NOW() WHERE key = $2")
        .bind(&body.value)
        .bind(&key)
        .execute(&state.db)
        .await;

    let status = match result {
        Ok(r) if r.rows_affected() > 0 => "ok",
        Ok(_) => {
            // Key doesn't exist, insert it
            let _ = sqlx::query(
                "INSERT INTO settings (key, value, category, description, is_secret) VALUES ($1, $2, 'custom', '', FALSE)",
            )
            .bind(&key)
            .bind(&body.value)
            .execute(&state.db)
            .await;
            "created"
        }
        Err(e) => return Json(serde_json::json!({ "status": "error", "error": e.to_string() })),
    };

    // Invalida cache DLP se è cambiata una chiave di configurazione DLP
    if matches!(key.as_str(), "dlp_enabled" | "dlp_allow_cloud_tier2" | "dlp_allow_cloud_tier3") {
        crate::dlp::invalidate_dlp_cache();
    }

    // Propaga impostazioni di connessione come variabili d'ambiente di processo
    // (effetto immediato per tutti i nuovi client nexus-http, no riavvio)
    match key.as_str() {
        "nexus_external_proxy" => {
            if body.value.is_empty() {
                std::env::remove_var("NEXUS_PROXY");
            } else {
                std::env::set_var("NEXUS_PROXY", &body.value);
            }
            // Notifica il Neural Core di ricaricare le impostazioni
            let neural_url = std::env::var("NEURAL_CORE_REST_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());
            let client = nexus_http::build_client();
            let _ = client.post(&format!("{}/reload-settings", neural_url))
                .json(&serde_json::json!({}))
                .send()
                .await;
        }
        "network_dns_servers" => {
            // Notifica il Neural Core di ricaricare le impostazioni (applica DNS override)
            let neural_url = std::env::var("NEURAL_CORE_REST_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());
            let client = nexus_http::build_client();
            let _ = client.post(&format!("{}/reload-settings", neural_url))
                .json(&serde_json::json!({}))
                .send()
                .await;
        }
        _ => {}
    }

    Json(serde_json::json!({ "status": status, "key": key }))
}

/// PUT /api/settings — bulk update
pub async fn bulk_update(
    State(state): State<super::AppState>,
    Json(body): Json<BulkUpdateRequest>,
) -> Json<serde_json::Value> {
    ensure_required_settings(&state).await;

    let mut updated = 0;
    let mut errors = Vec::new();

    for entry in &body.settings {
        match sqlx::query(
            "INSERT INTO settings (key, value, category, description, is_secret, updated_at) VALUES ($1, $2, 'custom', '', FALSE, NOW()) ON CONFLICT (key) DO UPDATE SET value = $2, updated_at = NOW()",
        )
        .bind(&entry.key)
        .bind(&entry.value)
        .execute(&state.db)
        .await
        {
            Ok(_) => updated += 1,
            Err(e) => errors.push(format!("{}: {}", entry.key, e)),
        }
    }

    // Se sono state cambiate chiavi DLP, invalida la cache in-process
    let has_dlp_key = body
        .settings
        .iter()
        .any(|e| matches!(e.key.as_str(), "dlp_enabled" | "dlp_allow_cloud_tier2" | "dlp_allow_cloud_tier3"));
    if has_dlp_key {
        crate::dlp::invalidate_dlp_cache();
    }

    // Se è stata salvata almeno una API key, ricarica automaticamente le chiavi nel brain
    let has_api_key = body.settings.iter().any(|e| e.key.ends_with("_api_key"));
    if has_api_key && errors.is_empty() {
        // Il brain REST server è su NEURAL_CORE_URL (porta 8001) o su /neural se proxied
        // Ma per il reload interno usiamo l'URL diretto interno
        let brain_url = std::env::var("NEURAL_CORE_REST_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());
        tokio::spawn(async move {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default();
            match client
                .post(format!("{brain_url}/reload-settings"))
                .json(&serde_json::json!({"mcp_core_url": "http://localhost:4000"}))
                .send()
                .await
            {
                Ok(r) => tracing::info!("Brain reload-settings: {}", r.status()),
                Err(e) => tracing::warn!("Brain reload-settings failed: {e}"),
            }
        });
    }

    Json(serde_json::json!({
        "status": if errors.is_empty() { "ok" } else { "partial" },
        "updated": updated,
        "errors": errors,
    }))
}

/// GET /internal/settings/:key — get raw value (internal use, not masked)
pub async fn get_raw_value(
    State(state): State<super::AppState>,
    Path(key): Path<String>,
) -> Json<serde_json::Value> {
    let value = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
        .bind(&key)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();

    Json(serde_json::json!({ "key": key, "value": value }))
}

/// Read a single setting value from the DB by key.
/// Returns `Ok(None)` if the key does not exist.
pub async fn get_setting(db: &PgPool, key: &str) -> anyhow::Result<Option<String>> {
    let value = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
        .bind(key)
        .fetch_optional(db)
        .await?;
    Ok(value)
}
