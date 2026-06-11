use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde_json::json;
use std::path::PathBuf;

// Tipi DTO: punto unico in nexus_types::settings_dto (regola L / ADR 0026, S8).
pub use nexus_types::settings_dto::{
    BulkUpdateRequest, CreateDirectoryRequest, FsBrowseQuery, Setting,
    UpdateSettingRequest,
};

// FS browse: punto unico in nexus_types::fs_browse (regola L / ADR 0026).
use nexus_types::fs_browse::{list_directories, list_root_candidates};
// Tipi e helper API: punto unico in nexus_types (regola L / ADR 0026, cluster E6).
// Prima `ApiError`/`ApiResult`/`api_error`/`validate_directory_name` erano
// ri-implementati identici qui e in crates/admin-service/src/settings.rs.
use nexus_types::{
    api_error, validate_directory_name_api as validate_directory_name, ApiError, ApiResult,
};

fn map_create_dir_error(error: std::io::Error) -> ApiError {
    let status = match error.kind() {
        std::io::ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
        std::io::ErrorKind::AlreadyExists => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    api_error(status, error.to_string())
}

async fn ensure_required_settings(state: &super::AppState) {
    // Default statici: seed via migrazione 0325 (regola G/H), niente piu' env
    // var (regola G) ne' INSERT ad-hoc all'avvio (regola H). Qui resta solo la
    // parte dinamica (projects_base_root da working dir), il cui punto unico e'
    // in nexus-types (prima era duplicata anche in admin-service).
    nexus_types::ensure_projects_base_root(&state.db).await;
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

/// Mascheramento valori secret per la response JSON: prima/ultime 4 lettere + `****`.
/// Punto unico (regola L, S23) per i 2 handler `list_settings` e `list_by_category`
/// che applicavano lo stesso identico mapping.
fn mask_settings(settings: Vec<Setting>) -> Vec<serde_json::Value> {
    settings
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
        .collect()
}

/// GET /api/settings — all settings (secrets are masked)
pub async fn list_settings(State(state): State<super::AppState>) -> Json<serde_json::Value> {
    ensure_required_settings(&state).await;

    // Fix S87: prima .unwrap_or_default() mostrava "0 settings" su DB down,
    // l'admin pensava di dover ripopolare. Ora logga + ritorna lista vuota
    // ma con flag che il chiamante puo' tracciare (regola H pragmatica:
    // signature Json<Value> non puo' diventare ApiResult senza rompere il
    // router; almeno l'errore appare nei log con livello WARN).
    let settings = match sqlx::query_as::<_, Setting>(
        "SELECT key, value, category, description, is_secret, updated_at FROM settings ORDER BY category, key",
    )
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("list_settings: SELECT settings fallito: {}", e);
            Vec::new()
        }
    };

    let masked = mask_settings(settings);

    Json(serde_json::json!({ "settings": masked }))
}

/// GET /api/settings/:category — settings filtered by category
pub async fn list_by_category(
    State(state): State<super::AppState>,
    Path(category): Path<String>,
) -> Json<serde_json::Value> {
    ensure_required_settings(&state).await;

    // Fix S87: vedi list_settings.
    let settings = match sqlx::query_as::<_, Setting>(
        "SELECT key, value, category, description, is_secret, updated_at FROM settings WHERE category = $1 ORDER BY key",
    )
    .bind(&category)
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("list_by_category({}): SELECT fallito: {}", category, e);
            Vec::new()
        }
    };

    let masked = mask_settings(settings);

    Json(serde_json::json!({ "settings": masked }))
}

/// GET /api/admin/settings-categories — categorie distinte con conteggio.
///
/// Fonte per la sidebar admin dinamica (regola L): le voci di navigazione
/// derivano dai DATI, non da una lista hardcoded nel frontend. Prima del
/// fix le categorie fuori dalla lista statica erano invisibili (160 chiavi
/// non amministrabili da UI).
pub async fn list_categories(State(state): State<super::AppState>) -> Json<serde_json::Value> {
    let rows: Vec<(String, i64)> = match sqlx::query_as(
        "SELECT category, count(*) FROM settings WHERE category <> '' GROUP BY category ORDER BY category",
    )
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("list_categories: SELECT fallito: {}", e);
            Vec::new()
        }
    };
    let categories: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(category, count)| serde_json::json!({ "category": category, "count": count }))
        .collect();
    Json(serde_json::json!({ "categories": categories }))
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

    // Notifica tutti i client connessi (evento system-wide)
    let ns = key.split('_').next().unwrap_or("admin").to_string();
    nexus_events::dispatcher::broadcast_all_global(nexus_events::ProjectEvent::SettingChanged {
        namespace: ns,
        key: key.clone(),
    });

    // Invalida cache DLP se è cambiata una chiave di configurazione DLP
    if matches!(
        key.as_str(),
        "dlp_enabled" | "dlp_allow_cloud_tier2" | "dlp_allow_cloud_tier3"
    ) {
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
            let _ = client
                .post(format!("{}/reload-settings", neural_url))
                .json(&serde_json::json!({}))
                .send()
                .await;
        }
        "network_dns_servers" => {
            // Notifica il Neural Core di ricaricare le impostazioni (applica DNS override)
            let neural_url = std::env::var("NEURAL_CORE_REST_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());
            let client = nexus_http::build_client();
            let _ = client
                .post(format!("{}/reload-settings", neural_url))
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
            Ok(_) => {
                updated += 1;
                // Notifica per ogni setting aggiornato
                let ns = entry.key.split('_').next().unwrap_or("admin").to_string();
                nexus_events::dispatcher::broadcast_all_global(
                    nexus_events::ProjectEvent::SettingChanged {
                        namespace: ns,
                        key: entry.key.clone(),
                    },
                );
            }
            Err(e) => errors.push(format!("{}: {}", entry.key, e)),
        }
    }

    // Se sono state cambiate chiavi DLP, invalida la cache in-process
    let has_dlp_key = body.settings.iter().any(|e| {
        matches!(
            e.key.as_str(),
            "dlp_enabled" | "dlp_allow_cloud_tier2" | "dlp_allow_cloud_tier3"
        )
    });
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
    // Lettura via punto unico (regola L / ADR 0026).
    // Fix S87: uso la variante _checked che propaga errori DB invece di
    // ingoiarli silenziosamente. Su Err logga + ritorna "".
    let value = match nexus_auth::get_setting_checked(&state.db, &key).await {
        Ok(opt) => opt.unwrap_or_default(),
        Err(e) => {
            tracing::warn!("get_raw_value({}): get_setting_checked fallito: {}", key, e);
            String::new()
        }
    };

    Json(serde_json::json!({ "key": key, "value": value }))
}

/// Lettura setting: punto unico in nexus-auth (regola L / ADR 0026).
/// Re-export con la firma storica (Result, valore raw, propaga l'errore DB).
pub use nexus_auth::get_setting_checked as get_setting;
