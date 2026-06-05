//! Configurazione DB del progetto e gestione connessioni.
//!
//! Route:
//!   GET    /api/projects/:id/db                                   -> get_project_db_config
//!   POST   /api/projects/:id/db/config                           -> set_project_db_config
//!   GET    /api/projects/:id/db/connections                      -> list_project_db_connections
//!   POST   /api/projects/:id/db/connections/:conn_id/set-primary -> set_primary_project_db_connection
//!   DELETE /api/projects/:id/db/connections/:conn_id             -> delete_project_db_connection

use axum::{
    extract::{Extension, Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use super::shared::{api_err, ApiResult};
type ApiError = (StatusCode, Json<Value>);
use crate::{auth::Claims, AppState};

// ── Strutture di risposta ────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ProjectDbConfigResponse {
    pub project_id: String,
    pub engine: Option<String>,
    pub hosting_mode: Option<String>,
    pub migration_tool: Option<String>,
    pub migration_path: Option<String>,
    pub allow_ddl_override: bool,
    pub detection_metadata: Value,
    pub pending_count: i64,
    pub applied_count: i64,
}

// ── Corpi richiesta ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SetDbConfigBody {
    pub engine: Option<String>,
    pub hosting_mode: Option<String>,
    pub migration_tool: Option<String>,
    pub migration_path: Option<String>,
    pub allow_ddl_override: Option<bool>,
    /// Stringa di connessione (cifrata lato client o passata in chiaro per Nexus-managed)
    pub connection_string: Option<String>,
    /// Nome logico della connessione; default "primary".
    pub name: Option<String>,
    /// Se true, imposta questa connessione come primaria (default true per la connessione
    /// "primary" o quando e' la prima registrata).
    pub is_primary: Option<bool>,
}

// ── GET /api/projects/:id/db ─────────────────────────────────────────────────

pub async fn get_project_db_config(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
) -> ApiResult {
    let row = sqlx::query(
        r#"
        SELECT
            engine, hosting_mode, migration_tool, migration_path,
            allow_ddl_override, detection_metadata
        FROM project_database_config
        WHERE project_id = $1
        ORDER BY is_primary DESC, LOWER(name)
        LIMIT 1
        "#,
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let (
        engine,
        hosting_mode,
        migration_tool,
        migration_path,
        allow_ddl_override,
        detection_metadata,
    ) = if let Some(r) = row {
        let allow: bool = r.try_get("allow_ddl_override").unwrap_or(false);
        let meta: Value = r
            .try_get::<serde_json::Value, _>("detection_metadata")
            .unwrap_or(json!({}));
        (
            r.try_get::<Option<String>, _>("engine").unwrap_or(None),
            r.try_get::<Option<String>, _>("hosting_mode")
                .unwrap_or(None),
            r.try_get::<Option<String>, _>("migration_tool")
                .unwrap_or(None),
            r.try_get::<Option<String>, _>("migration_path")
                .unwrap_or(None),
            allow,
            meta,
        )
    } else {
        (None, None, None, None, false, json!({}))
    };

    // Conteggi migrazioni
    let pending_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM project_migration_history WHERE project_id=$1 AND status='pending'",
    )
    .bind(project_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    let applied_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM project_migration_history WHERE project_id=$1 AND status='applied'",
    )
    .bind(project_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    Ok(Json(json!({
        "project_id": project_id.to_string(),
        "engine": engine,
        "hosting_mode": hosting_mode,
        "migration_tool": migration_tool,
        "migration_path": migration_path,
        "allow_ddl_override": allow_ddl_override,
        "detection_metadata": detection_metadata,
        "pending_count": pending_count,
        "applied_count": applied_count,
    })))
}

// ── POST /api/projects/:id/db/config ─────────────────────────────────────────

pub async fn set_project_db_config(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
    Json(body): Json<SetDbConfigBody>,
) -> ApiResult {
    // Verifica che il progetto esista e appartenga all'utente
    ensure_project_owner(&state, &claims, project_id).await?;

    // Cifra connection_string (uso base64 come placeholder — in prod usare la key manager)
    let secret_bytes: Option<Vec<u8>> = body.connection_string.as_deref().map(|s| {
        use std::io::Write;
        let mut buf = Vec::new();
        write!(buf, "{}", s).ok();
        buf
    });

    let name = body.name.as_deref().unwrap_or("primary").trim().to_string();
    if name.is_empty() {
        return Err(api_err(StatusCode::BAD_REQUEST, "Nome connessione vuoto"));
    }

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Se e' la prima connessione del progetto, is_primary default true.
    let existing_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM project_database_config WHERE project_id = $1")
            .bind(project_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let is_primary = body.is_primary.unwrap_or(existing_count == 0);

    // UPSERT manuale per gestire indice partial unique (project_id, LOWER(name)).
    let existing_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM project_database_config WHERE project_id = $1 AND LOWER(name) = LOWER($2)",
    )
    .bind(project_id)
    .bind(&name)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if is_primary {
        sqlx::query("UPDATE project_database_config SET is_primary = false WHERE project_id = $1")
            .bind(project_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    match existing_id {
        Some(_) => {
            sqlx::query(
                r#"
                UPDATE project_database_config SET
                    engine             = COALESCE($2, engine),
                    hosting_mode       = COALESCE($3, hosting_mode),
                    migration_tool     = COALESCE($4, migration_tool),
                    migration_path     = COALESCE($5, migration_path),
                    allow_ddl_override = COALESCE($6, allow_ddl_override),
                    connection_secret  = COALESCE($7, connection_secret),
                    is_primary         = $8,
                    updated_at         = NOW()
                WHERE project_id = $1 AND LOWER(name) = LOWER($9)
                "#,
            )
            .bind(project_id)
            .bind(&body.engine)
            .bind(&body.hosting_mode)
            .bind(&body.migration_tool)
            .bind(&body.migration_path)
            .bind(body.allow_ddl_override)
            .bind(secret_bytes.as_deref())
            .bind(is_primary)
            .bind(&name)
            .execute(&mut *tx)
            .await
            .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
        None => {
            sqlx::query(
                r#"
                INSERT INTO project_database_config
                    (project_id, name, engine, hosting_mode, migration_tool, migration_path,
                     allow_ddl_override, connection_secret, is_primary, detection_metadata)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, '{}'::jsonb)
                "#,
            )
            .bind(project_id)
            .bind(&name)
            .bind(&body.engine)
            .bind(&body.hosting_mode)
            .bind(&body.migration_tool)
            .bind(&body.migration_path)
            .bind(body.allow_ddl_override.unwrap_or(false))
            .bind(secret_bytes.as_deref())
            .bind(is_primary)
            .execute(&mut *tx)
            .await
            .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
    }

    tx.commit()
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Write-back: aggiorna la connection string nei file di configurazione del progetto
    // (appsettings.*.json, .env, ecc.) se fornita e se il progetto e` su disco locale.
    // Tutto il filesystem I/O e' spostato in spawn_blocking per non bloccare tokio.
    let mut writeback_error: Option<String> = None;
    if let Some(conn_str) = body
        .connection_string
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        let project_root: Option<String> =
            sqlx::query_scalar("SELECT repository_root_path FROM projects WHERE id=$1")
                .bind(project_id)
                .fetch_optional(&state.db)
                .await
                .ok()
                .flatten()
                .filter(|s: &String| !s.is_empty());
        if let Some(root) = project_root {
            let conn_str_owned = conn_str.to_string();
            let wb_result = tokio::task::spawn_blocking(move || {
                let root_path = std::path::Path::new(&root);
                if !root_path.exists() {
                    return None;
                }
                let mut wb_error: Option<String> = None;
                let candidates = ["appsettings.Development.json", "appsettings.json"];
                let mut config_files: Vec<std::path::PathBuf> = Vec::new();
                fn find_configs(
                    dir: &std::path::Path,
                    names: &[&str],
                    out: &mut Vec<std::path::PathBuf>,
                    depth: u8,
                ) {
                    if depth > 4 {
                        return;
                    }
                    let Ok(entries) = std::fs::read_dir(dir) else {
                        return;
                    };
                    for entry in entries.filter_map(|e| e.ok()) {
                        let path = entry.path();
                        let fname = entry.file_name().to_string_lossy().to_string();
                        if fname.starts_with('.')
                            || fname == "node_modules"
                            || fname == "bin"
                            || fname == "obj"
                            || fname == "target"
                        {
                            continue;
                        }
                        if path.is_file() && names.contains(&fname.as_str()) {
                            out.push(path);
                        } else if path.is_dir() {
                            find_configs(&path, names, out, depth + 1);
                        }
                    }
                }
                find_configs(root_path, &candidates, &mut config_files, 0);
                for config_file in &config_files {
                    match std::fs::read_to_string(config_file) {
                        Ok(content) => {
                            if let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(&content)
                            {
                                let updated = if let Some(cs) = doc.get_mut("ConnectionStrings") {
                                    if let Some(obj) = cs.as_object_mut() {
                                        for (_key, val) in obj.iter_mut() {
                                            *val =
                                                serde_json::Value::String(conn_str_owned.clone());
                                        }
                                        true
                                    } else {
                                        false
                                    }
                                } else {
                                    false
                                };
                                if updated {
                                    if let Ok(pretty) = serde_json::to_string_pretty(&doc) {
                                        if let Err(e) = std::fs::write(config_file, pretty + "\n") {
                                            tracing::warn!(
                                                "write-back {} fallito: {}",
                                                config_file.display(),
                                                e
                                            );
                                            wb_error = Some(format!(
                                                "Scrittura {} fallita: {}",
                                                config_file.display(),
                                                e
                                            ));
                                        } else {
                                            tracing::info!(
                                                "write-back connection string in {}",
                                                config_file.display()
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("lettura {} fallita: {}", config_file.display(), e);
                        }
                    }
                }
                let env_files = ["env", ".env", ".env.local", ".env.development"];
                for env_name in &env_files {
                    let env_path = root_path.join(env_name);
                    if env_path.is_file() {
                        if let Ok(content) = std::fs::read_to_string(&env_path) {
                            let mut lines: Vec<String> =
                                content.lines().map(String::from).collect();
                            let mut found = false;
                            for line in &mut lines {
                                let trimmed = line.trim();
                                if trimmed.starts_with('#') {
                                    continue;
                                }
                                if let Some((k, _)) = trimmed.split_once('=') {
                                    let kl = k.trim().to_lowercase();
                                    if kl.contains("database_url")
                                        || kl.contains("connection")
                                        || kl.contains("db_url")
                                    {
                                        *line = format!("{}={}", k.trim(), conn_str_owned);
                                        found = true;
                                    }
                                }
                            }
                            if found {
                                let _ = std::fs::write(&env_path, lines.join("\n") + "\n");
                                tracing::info!(
                                    "write-back connection string in {}",
                                    env_path.display()
                                );
                            }
                        }
                    }
                }
                wb_error
            })
            .await
            .unwrap_or(None);
            writeback_error = wb_result;
        }
    }

    let mut result = json!({ "ok": true, "name": name, "is_primary": is_primary });
    if let Some(wb_err) = writeback_error {
        result["writeback_warning"] = json!(wb_err);
    }
    Ok(Json(result))
}

// ── GET /api/projects/:id/db/connections ─────────────────────────────────────

pub async fn list_project_db_connections(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
) -> ApiResult {
    ensure_project_owner(&state, &claims, project_id).await?;

    // NB: NON filtriamo per hosting_mode. I DB 'internal' sono quelli
    // auto-provisionati da Nexus (es. <slug>_app sul container postgres-app
    // tramite ensure_project_db_url) e l'utente vuole vederli nel pannello.
    // Bug osservato 31/05/2026: filtro `hosting_mode <> 'internal'` nascondeva
    // il DB di Beauty-Book anche se registrato in project_database_config.
    let rows = sqlx::query(
        r#"
        SELECT id, name, engine, hosting_mode, migration_tool, migration_path,
               allow_ddl_override, is_primary, created_at, updated_at
        FROM project_database_config
        WHERE project_id = $1
        ORDER BY is_primary DESC, LOWER(name)
        "#,
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let connections: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.try_get::<Uuid, _>("id").ok().map(|u| u.to_string()),
                "name": r.try_get::<String, _>("name").unwrap_or_default(),
                "engine": r.try_get::<Option<String>, _>("engine").unwrap_or(None),
                "hosting_mode": r.try_get::<Option<String>, _>("hosting_mode").unwrap_or(None),
                "migration_tool": r.try_get::<Option<String>, _>("migration_tool").unwrap_or(None),
                "migration_path": r.try_get::<Option<String>, _>("migration_path").unwrap_or(None),
                "allow_ddl_override": r.try_get::<bool, _>("allow_ddl_override").unwrap_or(false),
                "is_primary": r.try_get::<bool, _>("is_primary").unwrap_or(false),
            })
        })
        .collect();

    Ok(Json(json!({ "connections": connections })))
}

/// Verifica che `claims.sub` sia il proprietario del progetto.
/// Punto unico (regola L, S55) per il pattern duplicato in set_primary +
/// delete connection handlers.
async fn ensure_project_owner(
    state: &AppState,
    claims: &Claims,
    project_id: Uuid,
) -> Result<(), ApiError> {
    let owner: Option<Uuid> = sqlx::query_scalar("SELECT owner_user_id FROM projects WHERE id=$1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let caller_uuid = Uuid::parse_str(&claims.sub)
        .map_err(|_| api_err(StatusCode::BAD_REQUEST, "Token utente non valido"))?;
    match owner {
        None => Err(api_err(StatusCode::NOT_FOUND, "Progetto non trovato")),
        Some(uid) if uid != caller_uuid => Err(api_err(StatusCode::FORBIDDEN, "Accesso negato")),
        _ => Ok(()),
    }
}

// ── POST /api/projects/:id/db/connections/:conn_id/set-primary ───────────────

pub async fn set_primary_project_db_connection(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((project_id, conn_id)): AxumPath<(Uuid, Uuid)>,
) -> ApiResult {
    ensure_project_owner(&state, &claims, project_id).await?;

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    sqlx::query("UPDATE project_database_config SET is_primary = false WHERE project_id = $1")
        .bind(project_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let updated = sqlx::query(
        "UPDATE project_database_config SET is_primary = true, updated_at = NOW() WHERE id = $1 AND project_id = $2",
    )
    .bind(conn_id)
    .bind(project_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if updated.rows_affected() == 0 {
        return Err(api_err(StatusCode::NOT_FOUND, "Connessione non trovata"));
    }

    tx.commit()
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "ok": true })))
}

// ── DELETE /api/projects/:id/db/connections/:conn_id ─────────────────────────

pub async fn delete_project_db_connection(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((project_id, conn_id)): AxumPath<(Uuid, Uuid)>,
) -> ApiResult {
    ensure_project_owner(&state, &claims, project_id).await?;

    // Impedisci di eliminare la primaria se esiste altra connessione non primaria.
    let is_primary: Option<bool> = sqlx::query_scalar(
        "SELECT is_primary FROM project_database_config WHERE id = $1 AND project_id = $2",
    )
    .bind(conn_id)
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let is_primary = match is_primary {
        Some(v) => v,
        None => return Err(api_err(StatusCode::NOT_FOUND, "Connessione non trovata")),
    };

    if is_primary {
        let others: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM project_database_config WHERE project_id = $1 AND id <> $2",
        )
        .bind(project_id)
        .bind(conn_id)
        .fetch_one(&state.db)
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        if others > 0 {
            return Err(api_err(
                StatusCode::BAD_REQUEST,
                "Imposta un'altra connessione come primaria prima di eliminare questa",
            ));
        }
    }

    sqlx::query("DELETE FROM project_database_config WHERE id = $1 AND project_id = $2")
        .bind(conn_id)
        .bind(project_id)
        .execute(&state.db)
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "ok": true })))
}
