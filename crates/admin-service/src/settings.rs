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
use nexus_types::{
    api_error, ApiError, ApiResult,
};

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

/// La sequenza (risolvi parent, valida nome, crea) vive in
/// `nexus_types::fs_browse::crea_directory`: era duplicata identica in
/// `mcp-core::settings`, e il censimento delle firme l'ha fatta emergere il
/// 2026-08-05. Qui resta la traduzione in HTTP — l'unica cosa che le due copie
/// avevano davvero di diverso.
pub async fn create_directory(Json(body): Json<CreateDirectoryRequest>) -> ApiResult {
    use nexus_types::fs_browse::{crea_directory, ErroreCreaDirectory};

    let target = crea_directory(&body.parent_path, &body.name).map_err(|e| match e {
        ErroreCreaDirectory::ParentNonRisolvibile => {
            api_error(StatusCode::BAD_REQUEST, "Percorso parent non valido")
        }
        ErroreCreaDirectory::ParentNonDirectory => {
            api_error(StatusCode::BAD_REQUEST, "Il parent non e' una directory")
        }
        ErroreCreaDirectory::NomeNonValido(motivo) => api_error(StatusCode::BAD_REQUEST, motivo),
        ErroreCreaDirectory::GiaEsistente => {
            api_error(StatusCode::CONFLICT, "Directory gia' esistente")
        }
        ErroreCreaDirectory::Io(io) => {
            let status = match io.kind() {
                std::io::ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
                std::io::ErrorKind::AlreadyExists => StatusCode::CONFLICT,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            api_error(status, io.to_string())
        }
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

/// PUT /api/admin/setting/:key (:4010) — copia gemella di quella in mcp-core.
///
/// La UI non passa di qui: `app/api/admin/setting/[key]/route.ts` proxya su
/// :4000, e in Next una route handler vince sul rewrite catch-all
/// `/api/admin/:path*` -> :4010 (i rewrite dichiarati in array sono afterFiles).
/// Verificato al wire, non dedotto: `/api/admin/settings-categories` — che
/// esiste solo in mcp-core — risponde 401 via :3000 e 404 qui, quindi le rotte
/// settings arrivano a :4000. Le rotte SENZA route handler (`/orchestrator/*`,
/// `/alignment/*`, ...) invece arrivano davvero qui.
///
/// La rotta resta e deve rispettare lo stesso contratto. La scrittura delega al punto unico
/// `nexus_auth::update_setting_value` (regola L), che porta con se' sia l'esito
/// via status HTTP (200/404/500, regola M) sia il divieto di creare chiavi
/// implicitamente.
pub async fn update_setting(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(body): Json<UpdateSettingRequest>,
) -> Result<Json<Value>, ApiError> {
    nexus_auth::update_setting_value(&state.db, &key, &body.value)
        .await
        .map_err(|e| api_error(e.status_code(), e.to_string()))?;
    Ok(Json(json!({ "status": "ok", "key": key })))
}

/// PUT /api/admin/settings (:4010) — gemella di quella in mcp-core.
///
/// Stesso contratto: aggiorna e non crea (punto unico, regola L), e l'esito e'
/// lo status HTTP (regola M) — 200 se tutte le chiavi passano, 500 se anche una
/// sola e' rifiutata, col messaggio pronto in `error`.
pub async fn bulk_update(
    State(state): State<AppState>,
    Json(body): Json<BulkUpdateRequest>,
) -> (StatusCode, Json<Value>) {
    ensure_required_settings(&state).await;

    let mut updated = 0;
    let mut errors = Vec::new();

    for entry in &body.settings {
        // Aggiorna, non crea: stesso punto unico del PUT singolo (regola L).
        // Prima era un `INSERT ... 'custom' ... ON CONFLICT DO UPDATE`, cioe' il
        // secondo vettore per le scritture inefficaci in categoria 'custom'.
        match nexus_auth::update_setting_value(&state.db, &entry.key, &entry.value).await {
            Ok(()) => updated += 1,
            Err(e) => errors.push(format!("{}: {}", entry.key, e)),
        }
    }

    // Nota: il brain Python e' stato eliminato; la cache delle chiavi API e'
    // ora gestita da mcp-core/nexus-gateway con TTL DB-driven (refresh entro
    // ~60s). Nessun side-effect HTTP da invalidare qui (era best-effort).

    if errors.is_empty() {
        return (
            StatusCode::OK,
            Json(json!({ "status": "ok", "updated": updated, "errors": [] })),
        );
    }

    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "status": "partial",
            "updated": updated,
            "errors": errors,
            "error": format!(
                "{} chiave/i su {} non salvate: {}",
                errors.len(),
                body.settings.len(),
                errors.join(" | ")
            ),
        })),
    )
}

/// GET /internal/settings/:key — valore non mascherato delle chiavi NON segrete.
///
/// Gemello di `mcp_core::settings::get_raw_value`: stessa rotta, stesso difetto,
/// stesso punto unico. La rotta e' montata fuori dal layer di auth e il servizio
/// ascolta su `0.0.0.0`, quindi leggeva in chiaro `jwt_secret` e le API key a
/// chiunque raggiungesse la porta. Il predicato "esponibile senza auth" vive in
/// `nexus_auth::get_setting_public` (regola L), non qui.
pub async fn get_raw_value(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> (StatusCode, Json<Value>) {
    match nexus_auth::get_setting_public(&state.db, &key).await {
        Ok(nexus_auth::PublicSettingRead::Value(value)) => {
            (StatusCode::OK, Json(json!({ "key": key, "value": value })))
        }
        Ok(nexus_auth::PublicSettingRead::Redacted) => (
            StatusCode::FORBIDDEN,
            Json(json!({
                "key": key,
                "error": "chiave segreta: non leggibile da una rotta senza autenticazione",
            })),
        ),
        Ok(nexus_auth::PublicSettingRead::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "key": key, "error": "chiave inesistente" })),
        ),
        Err(e) => {
            tracing::warn!("get_raw_value({}): lettura fallita: {}", key, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "key": key, "error": "lettura setting fallita" })),
            )
        }
    }
}
