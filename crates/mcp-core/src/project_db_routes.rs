//! Gestori HTTP per le API project-DB.
//!
//! Route montate in `main.rs`:
//!   GET  /api/projects/:id/db                    → get_project_db_config
//!   POST /api/projects/:id/db/config             → set_project_db_config
//!   GET  /api/projects/:id/db/migrations         → list_project_migrations
//!   POST /api/projects/:id/db/migrations/apply   → apply_project_migrations
//!   POST /api/projects/:id/db/migrations/rollback→ rollback_project_migration
//!   POST /api/projects/:id/db/override-request   → request_ddl_override
//!   POST /api/projects/:id/db/query              → execute_project_db_query
//!                                                  (pannello SQL frontend; thin
//!                                                  wrapper sopra
//!                                                  crate::project_db::exec)

use axum::{
    extract::{Extension, Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::project_db::exec::{archive_ddl, execute_query, QueryExecError};
use crate::{auth::Claims, AppState};

type ApiError = (StatusCode, Json<Value>);
type ApiResult = Result<Json<Value>, ApiError>;

fn api_err(code: StatusCode, msg: impl Into<String>) -> ApiError {
    (code, Json(json!({ "error": msg.into() })))
}

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

#[derive(Debug, Serialize)]
pub struct ProjectMigrationRow {
    pub id: String,
    pub filename: String,
    pub checksum: Option<String>,
    pub status: String,
    pub description: Option<String>,
    pub created_by_agent: Option<String>,
    pub created_at: String,
    pub applied_at: Option<String>,
    pub error_message: Option<String>,
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

#[derive(Debug, Deserialize)]
pub struct ApplyMigrationsBody {
    /// Se presente, applica solo questa migration per nome file
    pub filename: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OverrideRequestBody {
    pub sql: String,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct ExecuteQueryBody {
    /// Statement SQL (SELECT/INSERT/UPDATE/DELETE/DDL). Obbligatorio.
    pub sql: String,
    /// Parametri opzionali (array JSON). Bindati come TEXT; usare cast nel SQL
    /// per tipi non-stringa (es. `$1::int`).
    #[serde(default)]
    pub params: Vec<Value>,
    /// Limite righe ritornate per query read. Default 1000 (MAX_ROWS).
    #[serde(default)]
    pub max_rows: Option<usize>,
    /// Nome della connessione DB del progetto su cui eseguire (es. "primary",
    /// "analytics", "legacy_replica"). Se omesso o vuoto -> connessione con
    /// is_primary=true. Risolto in project_database_config.name.
    #[serde(default)]
    pub connection: Option<String>,
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

    let (engine, hosting_mode, migration_tool, migration_path, allow_ddl_override, detection_metadata) =
        if let Some(r) = row {
            let allow: bool = r.try_get("allow_ddl_override").unwrap_or(false);
            let meta: Value = r.try_get::<serde_json::Value, _>("detection_metadata")
                .unwrap_or(json!({}));
            (
                r.try_get::<Option<String>, _>("engine").unwrap_or(None),
                r.try_get::<Option<String>, _>("hosting_mode").unwrap_or(None),
                r.try_get::<Option<String>, _>("migration_tool").unwrap_or(None),
                r.try_get::<Option<String>, _>("migration_path").unwrap_or(None),
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
    let owner: Option<Uuid> = sqlx::query_scalar("SELECT owner_user_id FROM projects WHERE id=$1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let caller_uuid = Uuid::parse_str(&claims.sub)
        .map_err(|_| api_err(StatusCode::BAD_REQUEST, "Token utente non valido"))?;

    match owner {
        None => return Err(api_err(StatusCode::NOT_FOUND, "Progetto non trovato")),
        Some(uid) if uid != caller_uuid => {
            return Err(api_err(StatusCode::FORBIDDEN, "Accesso negato"))
        }
        _ => {}
    }

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
    let existing_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM project_database_config WHERE project_id = $1",
    )
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
        sqlx::query(
            "UPDATE project_database_config SET is_primary = false WHERE project_id = $1",
        )
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
    if let Some(conn_str) = body.connection_string.as_deref().filter(|s| !s.trim().is_empty()) {
        let project_root: Option<String> = sqlx::query_scalar("SELECT repository_root_path FROM projects WHERE id=$1")
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
                if !root_path.exists() { return None; }
                let mut wb_error: Option<String> = None;
                let candidates = ["appsettings.Development.json", "appsettings.json"];
                let mut config_files: Vec<std::path::PathBuf> = Vec::new();
                fn find_configs(dir: &std::path::Path, names: &[&str], out: &mut Vec<std::path::PathBuf>, depth: u8) {
                    if depth > 4 { return; }
                    let Ok(entries) = std::fs::read_dir(dir) else { return };
                    for entry in entries.filter_map(|e| e.ok()) {
                        let path = entry.path();
                        let fname = entry.file_name().to_string_lossy().to_string();
                        if fname.starts_with('.') || fname == "node_modules" || fname == "bin" || fname == "obj" || fname == "target" { continue; }
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
                            if let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(&content) {
                                let updated = if let Some(cs) = doc.get_mut("ConnectionStrings") {
                                    if let Some(obj) = cs.as_object_mut() {
                                        for (_key, val) in obj.iter_mut() {
                                            *val = serde_json::Value::String(conn_str_owned.clone());
                                        }
                                        true
                                    } else { false }
                                } else { false };
                                if updated {
                                    if let Ok(pretty) = serde_json::to_string_pretty(&doc) {
                                        if let Err(e) = std::fs::write(config_file, pretty + "\n") {
                                            tracing::warn!("write-back {} fallito: {}", config_file.display(), e);
                                            wb_error = Some(format!("Scrittura {} fallita: {}", config_file.display(), e));
                                        } else {
                                            tracing::info!("write-back connection string in {}", config_file.display());
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => { tracing::warn!("lettura {} fallita: {}", config_file.display(), e); }
                    }
                }
                let env_files = ["env", ".env", ".env.local", ".env.development"];
                for env_name in &env_files {
                    let env_path = root_path.join(env_name);
                    if env_path.is_file() {
                        if let Ok(content) = std::fs::read_to_string(&env_path) {
                            let mut lines: Vec<String> = content.lines().map(String::from).collect();
                            let mut found = false;
                            for line in &mut lines {
                                let trimmed = line.trim();
                                if trimmed.starts_with('#') { continue; }
                                if let Some((k, _)) = trimmed.split_once('=') {
                                    let kl = k.trim().to_lowercase();
                                    if kl.contains("database_url") || kl.contains("connection") || kl.contains("db_url") {
                                        *line = format!("{}={}", k.trim(), conn_str_owned);
                                        found = true;
                                    }
                                }
                            }
                            if found {
                                let _ = std::fs::write(&env_path, lines.join("\n") + "\n");
                                tracing::info!("write-back connection string in {}", env_path.display());
                            }
                        }
                    }
                }
                wb_error
            }).await.unwrap_or(None);
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
    let owner: Option<Uuid> = sqlx::query_scalar("SELECT owner_user_id FROM projects WHERE id=$1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let caller_uuid = Uuid::parse_str(&claims.sub)
        .map_err(|_| api_err(StatusCode::BAD_REQUEST, "Token utente non valido"))?;

    match owner {
        None => return Err(api_err(StatusCode::NOT_FOUND, "Progetto non trovato")),
        Some(uid) if uid != caller_uuid => {
            return Err(api_err(StatusCode::FORBIDDEN, "Accesso negato"))
        }
        _ => {}
    }

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

// ── POST /api/projects/:id/db/connections/:conn_id/set-primary ───────────────

pub async fn set_primary_project_db_connection(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((project_id, conn_id)): AxumPath<(Uuid, Uuid)>,
) -> ApiResult {
    let owner: Option<Uuid> = sqlx::query_scalar("SELECT owner_user_id FROM projects WHERE id=$1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let caller_uuid = Uuid::parse_str(&claims.sub)
        .map_err(|_| api_err(StatusCode::BAD_REQUEST, "Token utente non valido"))?;

    match owner {
        None => return Err(api_err(StatusCode::NOT_FOUND, "Progetto non trovato")),
        Some(uid) if uid != caller_uuid => {
            return Err(api_err(StatusCode::FORBIDDEN, "Accesso negato"))
        }
        _ => {}
    }

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
    let owner: Option<Uuid> = sqlx::query_scalar("SELECT owner_user_id FROM projects WHERE id=$1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let caller_uuid = Uuid::parse_str(&claims.sub)
        .map_err(|_| api_err(StatusCode::BAD_REQUEST, "Token utente non valido"))?;

    match owner {
        None => return Err(api_err(StatusCode::NOT_FOUND, "Progetto non trovato")),
        Some(uid) if uid != caller_uuid => {
            return Err(api_err(StatusCode::FORBIDDEN, "Accesso negato"))
        }
        _ => {}
    }

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

// ── GET /api/projects/:id/db/migrations ──────────────────────────────────────

pub async fn list_project_migrations(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
) -> ApiResult {
    let rows = sqlx::query(
        r#"
        SELECT
            id, filename, checksum, status, description,
            created_by_agent, created_at, applied_at, error_message
        FROM project_migration_history
        WHERE project_id = $1
        ORDER BY created_at ASC
        "#,
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let migrations: Vec<Value> = rows
        .iter()
        .map(|r| {
            let id: Uuid = r.get("id");
            let created_at: chrono::DateTime<chrono::Utc> = r.get("created_at");
            let applied_at: Option<chrono::DateTime<chrono::Utc>> =
                r.try_get("applied_at").unwrap_or(None);
            json!({
                "id": id.to_string(),
                "filename": r.get::<String, _>("filename"),
                "checksum": r.try_get::<Option<String>, _>("checksum").unwrap_or(None),
                "status": r.get::<String, _>("status"),
                "description": r.try_get::<Option<String>, _>("description").unwrap_or(None),
                "created_by_agent": r.try_get::<Option<String>, _>("created_by_agent").unwrap_or(None),
                "created_at": created_at.to_rfc3339(),
                "applied_at": applied_at.map(|t| t.to_rfc3339()),
                "error_message": r.try_get::<Option<String>, _>("error_message").unwrap_or(None),
            })
        })
        .collect();

    Ok(Json(json!({ "migrations": migrations })))
}

// ── POST /api/projects/:id/db/migrations/apply ───────────────────────────────

pub async fn apply_project_migrations(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
    body: Option<Json<ApplyMigrationsBody>>,
) -> ApiResult {
    let filename_filter = body.as_ref().and_then(|b| b.filename.as_deref());

    let pending_rows = sqlx::query(
        r#"
        SELECT id, filename, sql_diff, rollback_sql
        FROM project_migration_history
        WHERE project_id = $1
          AND status = 'pending'
        ORDER BY created_at ASC
        "#,
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Ottieni URL DB del progetto
    let db_url = resolve_project_db_url(project_id);

    let project_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&db_url)
        .await
        .map_err(|e| api_err(StatusCode::BAD_GATEWAY, format!("Connessione DB progetto fallita: {e}")))?;

    let mut applied = Vec::new();
    let mut errors = Vec::new();

    for row in &pending_rows {
        let migration_id: Uuid = row.get("id");
        let filename: String = row.get("filename");
        let sql_diff: Option<String> = row.try_get("sql_diff").unwrap_or(None);

        if let Some(filter) = filename_filter {
            if filename != filter {
                continue;
            }
        }

        let sql_to_run = match sql_diff {
            Some(ref s) if !s.trim().is_empty() => s.clone(),
            _ => {
                errors.push(json!({ "filename": &filename, "error": "sql_diff mancante" }));
                sqlx::query(
                    "UPDATE project_migration_history SET status='failed', error_message=$2, applied_at=NOW() WHERE id=$1"
                )
                .bind(migration_id)
                .bind("sql_diff mancante")
                .execute(&state.db)
                .await
                .ok();
                continue;
            }
        };

        match sqlx::raw_sql(&sql_to_run).execute(&project_pool).await {
            Ok(_) => {
                let caller_uuid = Uuid::parse_str(&claims.sub).ok();
                let _ = sqlx::query(
                    r#"
                    UPDATE project_migration_history
                    SET status='applied', applied_at=NOW(), applied_by_user=$2, error_message=NULL
                    WHERE id=$1
                    "#,
                )
                .bind(migration_id)
                .bind(caller_uuid)
                .execute(&state.db)
                .await;
                applied.push(filename.clone());
            }
            Err(e) => {
                let err_str = e.to_string();
                let _ = sqlx::query(
                    "UPDATE project_migration_history SET status='failed', error_message=$2, applied_at=NOW() WHERE id=$1"
                )
                .bind(migration_id)
                .bind(&err_str)
                .execute(&state.db)
                .await;
                errors.push(json!({ "filename": &filename, "error": err_str }));
            }
        }
    }

    let ok = errors.is_empty();
    Ok(Json(json!({
        "ok": ok,
        "applied": applied,
        "errors": errors,
    })))
}

// ── POST /api/projects/:id/db/migrations/rollback ────────────────────────────

pub async fn rollback_project_migration(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
) -> ApiResult {
    // Trova l'ultima migration applicata
    let last = sqlx::query(
        r#"
        SELECT id, filename, rollback_sql
        FROM project_migration_history
        WHERE project_id = $1 AND status = 'applied'
        ORDER BY applied_at DESC
        LIMIT 1
        "#,
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let row = match last {
        None => return Ok(Json(json!({ "ok": false, "error": "Nessuna migration applicata da rollbackare" }))),
        Some(r) => r,
    };

    let migration_id: Uuid = row.get("id");
    let filename: String = row.get("filename");
    let rollback_sql: Option<String> = row.try_get("rollback_sql").unwrap_or(None);

    if let Some(sql) = rollback_sql {
        if !sql.trim().is_empty() {
            let db_url = resolve_project_db_url(project_id);
            let project_pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(2)
                .acquire_timeout(std::time::Duration::from_secs(10))
                .connect(&db_url)
                .await
                .map_err(|e| api_err(StatusCode::BAD_GATEWAY, format!("Connessione DB progetto fallita: {e}")))?;

            sqlx::raw_sql(&sql)
                .execute(&project_pool)
                .await
                .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("Rollback SQL fallito: {e}")))?;
        }
    }

    let caller_uuid = Uuid::parse_str(&claims.sub).ok();
    sqlx::query(
        r#"
        UPDATE project_migration_history
        SET status='rolled_back', applied_by_user=$2, applied_at=NOW()
        WHERE id=$1
        "#,
    )
    .bind(migration_id)
    .bind(caller_uuid)
    .execute(&state.db)
    .await
    .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "ok": true, "rolled_back": filename })))
}

// ── POST /api/projects/:id/db/override-request ───────────────────────────────

pub async fn request_ddl_override(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
    Json(body): Json<OverrideRequestBody>,
) -> ApiResult {
    if body.sql.trim().is_empty() {
        return Err(api_err(StatusCode::BAD_REQUEST, "sql obbligatorio"));
    }
    if body.reason.trim().len() < 10 {
        return Err(api_err(StatusCode::BAD_REQUEST, "reason deve avere almeno 10 caratteri"));
    }

    // Verifica che allow_ddl_override sia true
    let allow: Option<bool> = sqlx::query_scalar(
        "SELECT allow_ddl_override FROM project_database_config WHERE project_id=$1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .flatten();

    if allow != Some(true) {
        return Err(api_err(
            StatusCode::FORBIDDEN,
            "Override DDL non abilitato per questo progetto. Abilita allow_ddl_override prima.",
        ));
    }

    let caller_uuid = Uuid::parse_str(&claims.sub).ok();

    // Calcola checksum del SQL
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    body.sql.hash(&mut h);
    let checksum = format!("{:016x}", h.finish());

    let now = chrono::Utc::now();
    let filename = format!("override_{}_{}.sql", now.format("%Y%m%d_%H%M%S"), &checksum[..8]);

    // Inserisce con status pending_override
    let migration_id = sqlx::query_scalar::<_, Uuid>(
        r#"
        INSERT INTO project_migration_history
            (project_id, filename, checksum, status, sql_diff, override_reason, created_by_user, created_at)
        VALUES ($1, $2, $3, 'pending_override', $4, $5, $6, NOW())
        RETURNING id
        "#,
    )
    .bind(project_id)
    .bind(&filename)
    .bind(&checksum)
    .bind(&body.sql)
    .bind(&body.reason)
    .bind(caller_uuid)
    .fetch_one(&state.db)
    .await
    .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Esegui il DDL direttamente (l'approvazione UI è già avvenuta nel front-end tramite OverrideConfirmDialog)
    let db_url = resolve_project_db_url(project_id);
    match sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&db_url)
        .await
    {
        Ok(project_pool) => {
            match sqlx::raw_sql(&body.sql).execute(&project_pool).await {
                Ok(_) => {
                    let _ = sqlx::query(
                        r#"
                        UPDATE project_migration_history
                        SET status='overridden', applied_at=NOW(), applied_by_user=$2
                        WHERE id=$1
                        "#,
                    )
                    .bind(migration_id)
                    .bind(caller_uuid)
                    .execute(&state.db)
                    .await;
                }
                Err(e) => {
                    let _ = sqlx::query(
                        "UPDATE project_migration_history SET status='failed', error_message=$2 WHERE id=$1"
                    )
                    .bind(migration_id)
                    .bind(e.to_string())
                    .execute(&state.db)
                    .await;
                    return Err(api_err(StatusCode::INTERNAL_SERVER_ERROR, format!("DDL override fallito: {e}")));
                }
            }
        }
        Err(e) => {
            // Lascia il record in pending_override — l'admin può ritentare
            tracing::warn!("Override DDL: impossibile connettersi al DB del progetto {}: {}", project_id, e);
            return Ok(Json(json!({
                "ok": false,
                "pending_override_id": migration_id.to_string(),
                "warning": format!("Impossibile connettersi al DB: {e}. Il record è salvato come pending_override."),
            })));
        }
    }

    Ok(Json(json!({
        "ok": true,
        "migration_id": migration_id.to_string(),
        "filename": filename,
    })))
}

// ── Utility ──────────────────────────────────────────────────────────────────

/// Risolve l'URL del DB del progetto utente.
/// Controlla la variabile d'ambiente `PROJECT_{uuid_simple}_DB_URL`,
/// altrimenti usa il nome container Docker convenzionale.
fn resolve_project_db_url(project_id: Uuid) -> String {
    let env_key = format!("PROJECT_{}_DB_URL", project_id.simple());
    std::env::var(&env_key).unwrap_or_else(|_| {
        format!("postgresql://nexus:nexus@proj-{}-db:5432/app", project_id.simple())
    })
}

/// Converte una stringa di connessione in formato ADO.NET/Npgsql
/// (`Host=...;Port=...;Database=...;Username=...;Password=...`)
/// nel formato URL PostgreSQL richiesto da sqlx (`postgres://user:pass@host:port/db`).
/// Se la stringa e` gia` in formato URL, la restituisce invariata.
fn normalize_pg_connection_string(raw: &str) -> String {
    let trimmed = raw.trim();
    // Se e` gia` un URL postgres, ritorna invariata
    if trimmed.starts_with("postgres://") || trimmed.starts_with("postgresql://") {
        return trimmed.to_string();
    }
    // Parsing dei parametri ADO.NET (key=value separati da ;)
    let mut host = "localhost";
    let mut port = "5432";
    let mut database = "postgres";
    let mut username = "postgres";
    let mut password = "";
    let mut ssl_mode = "";
    for part in trimmed.split(';') {
        let part = part.trim();
        if part.is_empty() { continue; }
        let Some((k, v)) = part.split_once('=') else { continue };
        let key_lower = k.trim().to_lowercase();
        let val = v.trim();
        match key_lower.as_str() {
            "host" | "server" | "data source" => host = val,
            "port" => port = val,
            "database" | "initial catalog" | "db" => database = val,
            "username" | "user id" | "user" | "uid" => username = val,
            "password" | "pwd" => password = val,
            "sslmode" | "ssl mode" => ssl_mode = val,
            _ => {}
        }
    }
    let encoded_pass = urlencoding::encode(password);
    let mut url = format!("postgres://{}:{}@{}:{}/{}", username, encoded_pass, host, port, database);
    if !ssl_mode.is_empty() {
        url.push_str(&format!("?sslmode={}", ssl_mode));
    }
    url
}

// ── POST /api/projects/:id/db/detect ─────────────────────────────────────────

#[derive(Debug, Default, Serialize)]
struct DetectionResult {
    engine: Option<String>,
    migration_tool: Option<String>,
    migration_path: Option<String>,
    connection_string: Option<String>,
    hosting_mode: Option<String>,
    hints: Vec<String>,
    evidence: Vec<Value>,
}

fn read_text(path: &std::path::Path, max_bytes: usize) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::with_capacity(max_bytes.min(65536));
    f.take(max_bytes as u64).read_to_end(&mut buf).ok()?;
    String::from_utf8(buf).ok()
}

fn detect_from_env_content(content: &str) -> Option<(String, String)> {
    // Prima cerca URL completi (DATABASE_URL, POSTGRES_URL, ecc.)
    let mut env_vars: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        let Some((k, v)) = line.split_once('=') else { continue };
        let k = k.trim();
        let v = v.trim().trim_matches('"').trim_matches('\'');
        if v.is_empty() { continue; }
        env_vars.insert(k.to_ascii_uppercase(), v.to_string());

        let engine = if v.starts_with("postgres://") || v.starts_with("postgresql://") {
            "postgres"
        } else if v.starts_with("mysql://") || v.starts_with("mariadb://") {
            "mysql"
        } else if v.starts_with("sqlite:") {
            "sqlite"
        } else if v.starts_with("mongodb://") || v.starts_with("mongodb+srv://") {
            "mongodb"
        } else {
            continue;
        };
        let upper = k.to_ascii_uppercase();
        if upper.contains("DATABASE_URL") || upper.contains("POSTGRES_URL")
            || upper.contains("MYSQL_URL") || upper.contains("DB_URL")
            || upper.contains("MONGO_URL") || upper.contains("MONGODB_URI")
        {
            return Some((engine.to_string(), v.to_string()));
        }
    }

    // Fallback: costruisci connection string da variabili separate (POSTGRES_*, DB_*, ecc.)
    if let Some(conn) = build_connection_from_env_vars(&env_vars) {
        return Some(conn);
    }

    None
}

/// Costruisce una connection string da variabili d'ambiente separate
/// come POSTGRES_HOST, POSTGRES_PORT, POSTGRES_DB, POSTGRES_USER, POSTGRES_PASSWORD
/// o varianti come DB_HOST, PGHOST, MYSQL_HOST, ecc.
fn build_connection_from_env_vars(vars: &std::collections::HashMap<String, String>) -> Option<(String, String)> {
    // Pattern di variabili per engine noti
    struct EnvPattern {
        engine: &'static str,
        host_keys: &'static [&'static str],
        port_keys: &'static [&'static str],
        db_keys: &'static [&'static str],
        user_keys: &'static [&'static str],
        pass_keys: &'static [&'static str],
        default_port: &'static str,
    }

    let patterns = [
        EnvPattern {
            engine: "postgres",
            host_keys: &["POSTGRES_HOST", "PGHOST", "DB_HOST", "DATABASE_HOST"],
            port_keys: &["POSTGRES_PORT", "PGPORT", "DB_PORT", "DATABASE_PORT"],
            db_keys: &["POSTGRES_DB", "PGDATABASE", "DB_NAME", "DATABASE_NAME", "POSTGRES_DATABASE"],
            user_keys: &["POSTGRES_USER", "PGUSER", "DB_USER", "DATABASE_USER", "POSTGRES_USERNAME"],
            pass_keys: &["POSTGRES_PASSWORD", "PGPASSWORD", "DB_PASSWORD", "DATABASE_PASSWORD", "POSTGRES_PASS"],
            default_port: "5432",
        },
        EnvPattern {
            engine: "mysql",
            host_keys: &["MYSQL_HOST", "DB_HOST", "DATABASE_HOST"],
            port_keys: &["MYSQL_PORT", "DB_PORT", "DATABASE_PORT"],
            db_keys: &["MYSQL_DATABASE", "MYSQL_DB", "DB_NAME", "DATABASE_NAME"],
            user_keys: &["MYSQL_USER", "MYSQL_USERNAME", "DB_USER"],
            pass_keys: &["MYSQL_PASSWORD", "MYSQL_PASS", "DB_PASSWORD"],
            default_port: "3306",
        },
    ];

    for pat in &patterns {
        let host = pat.host_keys.iter().find_map(|k| vars.get(*k));
        let db = pat.db_keys.iter().find_map(|k| vars.get(*k));

        // Serve almeno host + database per costruire una connection string utile
        if let (Some(host), Some(db)) = (host, db) {
            let port = pat.port_keys.iter().find_map(|k| vars.get(*k))
                .map(|s| s.as_str())
                .unwrap_or(pat.default_port);
            let user = pat.user_keys.iter().find_map(|k| vars.get(*k))
                .map(|s| s.as_str())
                .unwrap_or("");
            let pass = pat.pass_keys.iter().find_map(|k| vars.get(*k))
                .map(|s| s.as_str())
                .unwrap_or("");

            let conn_str = if !user.is_empty() && !pass.is_empty() {
                format!("{}://{}:{}@{}:{}/{}", pat.engine, user, pass, host, port, db)
            } else if !user.is_empty() {
                format!("{}://{}@{}:{}/{}", pat.engine, user, host, port, db)
            } else {
                format!("{}://{}:{}/{}", pat.engine, host, port, db)
            };

            return Some((pat.engine.to_string(), conn_str));
        }
    }

    None
}

fn scan_project_db(root: &std::path::Path) -> DetectionResult {
    let mut r = DetectionResult::default();

    // 1) .env files
    for name in [".env", ".env.local", ".env.development", ".env.example"] {
        let p = root.join(name);
        if let Some(content) = read_text(&p, 64 * 1024) {
            if let Some((engine, url)) = detect_from_env_content(&content) {
                r.evidence.push(json!({"file": name, "matched": true}));
                r.hints.push(format!("{name}: rilevato {engine}"));
                if r.engine.is_none() { r.engine = Some(engine); }
                if r.connection_string.is_none() { r.connection_string = Some(url); }
            }
        }
    }

    // 2) docker-compose
    for name in ["docker-compose.yml", "docker-compose.yaml", "compose.yml", "compose.yaml"] {
        let p = root.join(name);
        if let Some(content) = read_text(&p, 128 * 1024) {
            let lc = content.to_ascii_lowercase();
            if lc.contains("image: postgres") || lc.contains("image: \"postgres") {
                if r.engine.is_none() { r.engine = Some("postgres".into()); }
                r.hints.push(format!("{name}: servizio postgres"));
                if r.hosting_mode.is_none() { r.hosting_mode = Some("internal".into()); }
            }
            if lc.contains("image: mysql") || lc.contains("image: mariadb") {
                if r.engine.is_none() { r.engine = Some("mysql".into()); }
                r.hints.push(format!("{name}: servizio mysql/mariadb"));
                if r.hosting_mode.is_none() { r.hosting_mode = Some("internal".into()); }
            }
        }
    }

    // 3) Migration tools
    if root.join("prisma/schema.prisma").exists() {
        r.migration_tool = Some("prisma".into());
        r.migration_path = Some("prisma/migrations".into());
        r.hints.push("Prisma schema rilevato".into());
        if let Some(content) = read_text(&root.join("prisma/schema.prisma"), 64 * 1024) {
            if content.contains("provider = \"postgresql\"") && r.engine.is_none() {
                r.engine = Some("postgres".into());
            } else if content.contains("provider = \"mysql\"") && r.engine.is_none() {
                r.engine = Some("mysql".into());
            } else if content.contains("provider = \"sqlite\"") && r.engine.is_none() {
                r.engine = Some("sqlite".into());
            }
        }
    }
    if root.join("alembic.ini").exists() {
        r.migration_tool = Some("alembic".into());
        r.migration_path = Some("alembic/versions".into());
        r.hints.push("Alembic rilevato".into());
    }
    for knex in ["knexfile.js", "knexfile.ts", "knexfile.cjs", "knexfile.mjs"] {
        if root.join(knex).exists() {
            r.migration_tool = Some("knex".into());
            r.migration_path = Some("migrations".into());
            r.hints.push(format!("Knex: {knex}"));
            break;
        }
    }
    if root.join("flyway.conf").exists() || root.join("conf/flyway.conf").exists() {
        r.migration_tool = Some("flyway".into());
        r.migration_path = Some("db/migration".into());
        r.hints.push("Flyway rilevato".into());
    }
    for dir in ["migrations", "db/migrations", "database/migrations", "sql/migrations"] {
        if root.join(dir).is_dir() {
            if r.migration_path.is_none() {
                r.migration_path = Some(dir.into());
            }
            r.hints.push(format!("Cartella migration: {dir}"));
            break;
        }
    }

    // 4) package.json dependencies
    if let Some(content) = read_text(&root.join("package.json"), 128 * 1024) {
        let lc = content.to_ascii_lowercase();
        if lc.contains("\"prisma\"") && r.migration_tool.is_none() { r.migration_tool = Some("prisma".into()); }
        if lc.contains("\"knex\"") && r.migration_tool.is_none() { r.migration_tool = Some("knex".into()); }
        if lc.contains("\"typeorm\"") && r.migration_tool.is_none() { r.migration_tool = Some("generic_sql".into()); r.hints.push("TypeORM rilevato".into()); }
        if lc.contains("\"pg\"") && r.engine.is_none() { r.engine = Some("postgres".into()); r.hints.push("dep pg".into()); }
        if (lc.contains("\"mysql2\"") || lc.contains("\"mysql\"")) && r.engine.is_none() {
            r.engine = Some("mysql".into()); r.hints.push("dep mysql".into());
        }
    }

    // 5) pyproject/requirements
    for f in ["pyproject.toml", "requirements.txt", "Pipfile"] {
        if let Some(content) = read_text(&root.join(f), 64 * 1024) {
            let lc = content.to_ascii_lowercase();
            if lc.contains("alembic") && r.migration_tool.is_none() { r.migration_tool = Some("alembic".into()); }
            if (lc.contains("psycopg") || lc.contains("asyncpg")) && r.engine.is_none() {
                r.engine = Some("postgres".into()); r.hints.push(format!("{f}: driver postgres"));
            }
            if lc.contains("pymysql") && r.engine.is_none() {
                r.engine = Some("mysql".into()); r.hints.push(format!("{f}: driver mysql"));
            }
        }
    }

    // 6) Cargo.toml
    if let Some(content) = read_text(&root.join("Cargo.toml"), 64 * 1024) {
        let lc = content.to_ascii_lowercase();
        if lc.contains("sqlx") || lc.contains("diesel") {
            if lc.contains("postgres") && r.engine.is_none() { r.engine = Some("postgres".into()); }
            if lc.contains("mysql") && r.engine.is_none() { r.engine = Some("mysql".into()); }
            if lc.contains("sqlite") && r.engine.is_none() { r.engine = Some("sqlite".into()); }
            r.hints.push("Cargo.toml: driver DB rilevato".into());
        }
    }

    // 7) .NET / ASP.NET Core — appsettings.json e *.csproj
    if r.engine.is_none() {
        // Leggi appsettings.Development.json poi appsettings.json
        for settings in ["appsettings.Development.json", "appsettings.json"] {
            let candidates = [
                root.join(settings),
                root.join("backend").join("FreeLance.Api").join(settings),
                root.join("src").join(settings),
                root.join("Api").join(settings),
            ];
            for candidate in &candidates {
                if let Some(content) = read_text(candidate, 64 * 1024) {
                    for line in content.lines() {
                        let line = line.trim();
                        if !line.contains("Connection") || !line.contains(':') { continue; }
                        let value = line.split(':').skip(1).collect::<Vec<_>>().join(":").trim().to_string();
                        let value = value.trim_matches('"').trim_matches(',').trim_matches('"');
                        let lc = value.to_ascii_lowercase();

                        // Helper di classificazione: priorita' ai segnali univoci
                        // di Postgres (Host=, Port=5432, postgres://) PRIMA di SQL Server.
                        // Necessario perche' Npgsql usa "Server=host;Port=5432;Database=...",
                        // stessi token usati da SQL Server (Server=host,1433;Database=...).
                        let detected: Option<&'static str> = if
                            lc.contains("postgresql://") || lc.contains("postgres://")
                        {
                            Some("postgres")
                        } else if lc.contains("host=") {
                            Some("postgres")
                        } else if lc.contains("port=5432") {
                            Some("postgres")
                        } else if lc.contains("port=3306") {
                            Some("mysql")
                        } else if lc.contains("mysql://") {
                            Some("mysql")
                        } else if lc.contains("initial catalog=") {
                            // Univocamente SQL Server
                            Some("sqlserver")
                        } else if lc.contains("server=") && (lc.contains(",1433") || lc.contains(",1434")) {
                            // Sintassi SQL Server con porta inline
                            Some("sqlserver")
                        } else if lc.contains(";port=") || lc.starts_with("port=") {
                            // `Port=` keyword separato (non SQL Server) ma porta non 5432/3306
                            // -> probabile Postgres su porta non standard
                            Some("postgres")
                        } else if lc.contains("server=") && lc.contains("database=") {
                            // Fallback legacy: nessun segnale Postgres/MySQL trovato
                            Some("sqlserver")
                        } else {
                            None
                        };

                        match detected {
                            Some("postgres") => {
                                r.engine = Some("postgres".into());
                                r.hints.push(format!("{}: rilevato PostgreSQL", settings));
                                if r.connection_string.is_none() {
                                    r.connection_string = Some(value.to_string());
                                }
                                break;
                            }
                            Some("mysql") => {
                                r.engine = Some("mysql".into());
                                r.hints.push(format!("{}: rilevato MySQL", settings));
                                if r.connection_string.is_none() {
                                    r.connection_string = Some(value.to_string());
                                }
                                break;
                            }
                            Some("sqlserver") => {
                                r.engine = Some("sqlserver".into());
                                r.hints.push(format!("{}: rilevato SQL Server", settings));
                                if r.connection_string.is_none() {
                                    r.connection_string = Some(value.to_string());
                                }
                                break;
                            }
                            _ => {}
                        }
                    }
                    if r.engine.is_some() { break; }
                }
            }
            if r.engine.is_some() { break; }
        }
    }

    // 8) *.csproj — PackageReference EF Core provider
    if r.engine.is_none() {
        'csproj: for search_dir in [root, &root.join("backend"), &root.join("src")] {
            if let Ok(entries) = std::fs::read_dir(search_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if !name.ends_with(".csproj") { continue; }
                    if let Some(content) = read_text(&entry.path(), 32 * 1024) {
                        let lc = content.to_ascii_lowercase();
                        if lc.contains("entityframeworkcore.sqlserver") || lc.contains("microsoft.data.sqlclient") {
                            r.engine = Some("sqlserver".into());
                            r.hints.push(format!("{name}: EF Core SQL Server"));
                            break 'csproj;
                        }
                        if lc.contains("npgsql.entityframeworkcore.postgresql") {
                            r.engine = Some("postgres".into());
                            r.hints.push(format!("{name}: EF Core PostgreSQL (Npgsql)"));
                            break 'csproj;
                        }
                        if lc.contains("pomelo.entityframeworkcore.mysql") {
                            r.engine = Some("mysql".into());
                            r.hints.push(format!("{name}: EF Core MySQL"));
                            break 'csproj;
                        }
                        if lc.contains("microsoft.entityframeworkcore.sqlite") {
                            r.engine = Some("sqlite".into());
                            r.hints.push(format!("{name}: EF Core SQLite"));
                            break 'csproj;
                        }
                    }
                }
            }
            // Cerca anche un livello più in profondità
            if let Ok(subdirs) = std::fs::read_dir(search_dir) {
                for sub in subdirs.flatten() {
                    if !sub.file_type().map(|t| t.is_dir()).unwrap_or(false) { continue; }
                    if let Ok(entries) = std::fs::read_dir(sub.path()) {
                        for entry in entries.flatten() {
                            let name = entry.file_name().to_string_lossy().to_string();
                            if !name.ends_with(".csproj") { continue; }
                            if let Some(content) = read_text(&entry.path(), 32 * 1024) {
                                let lc = content.to_ascii_lowercase();
                                if lc.contains("entityframeworkcore.sqlserver") || lc.contains("microsoft.data.sqlclient") {
                                    r.engine = Some("sqlserver".into());
                                    r.hints.push(format!("{name}: EF Core SQL Server"));
                                    break 'csproj;
                                }
                                if lc.contains("npgsql.entityframeworkcore.postgresql") {
                                    r.engine = Some("postgres".into());
                                    r.hints.push(format!("{name}: EF Core PostgreSQL (Npgsql)"));
                                    break 'csproj;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if r.migration_tool.is_none() { r.migration_tool = Some("generic_sql".into()); }
    if r.migration_path.is_none() { r.migration_path = Some("migrations".into()); }
    if r.hosting_mode.is_none() { r.hosting_mode = Some("external".into()); }
    r
}

pub async fn detect_project_db(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
) -> ApiResult {
    let root_path: Option<String> = sqlx::query_scalar(
        r#"SELECT COALESCE(r.root_path, p.analysis_json->>'rootPath', '')
           FROM projects p LEFT JOIN repositories r ON r.project_id = p.id
           WHERE p.id = $1"#,
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let root_path = root_path.unwrap_or_default();
    if root_path.is_empty() {
        return Err(api_err(StatusCode::BAD_REQUEST, "Root path progetto non disponibile. Rianalizza il progetto."));
    }
    let root = std::path::PathBuf::from(&root_path);
    if !root.is_dir() {
        return Err(api_err(StatusCode::BAD_REQUEST, format!("Root path non trovato: {}", root.display())));
    }

    let result = tokio::task::spawn_blocking(move || scan_project_db(&root))
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Salva metadata (merge in detection_metadata, senza sovrascrivere config esistente)
    let meta = serde_json::to_value(&result).unwrap_or(json!({}));
    let _ = sqlx::query(
        r#"
        INSERT INTO project_database_config (project_id, detection_metadata)
        VALUES ($1, $2)
        ON CONFLICT (project_id) DO UPDATE SET
            detection_metadata = EXCLUDED.detection_metadata,
            updated_at = NOW()
        "#,
    )
    .bind(project_id)
    .bind(&meta)
    .execute(&state.db)
    .await;

    Ok(Json(json!({
        "ok": true,
        "engine": result.engine,
        "migration_tool": result.migration_tool,
        "migration_path": result.migration_path,
        "connection_string": result.connection_string,
        "hosting_mode": result.hosting_mode,
        "hints": result.hints,
    })))
}

// ── POST /api/projects/:id/db/test-connection ────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TestConnectionBody {
    pub engine: Option<String>,
    pub connection_string: Option<String>,
    /// Identifica la connessione salvata da testare (per name logico o id).
    pub name: Option<String>,
    pub connection_id: Option<Uuid>,
}

pub async fn test_project_db_connection(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
    Json(body): Json<TestConnectionBody>,
) -> ApiResult {
    // URL: dal body (override esplicito) oppure dalla connessione salvata
    // individuata da connection_id / name / primary.
    let url = if let Some(u) = body.connection_string.as_deref().filter(|s| !s.trim().is_empty()) {
        u.to_string()
    } else {
        let saved: Option<Vec<u8>> = if let Some(id) = body.connection_id {
            sqlx::query_scalar(
                "SELECT connection_secret FROM project_database_config WHERE project_id=$1 AND id=$2",
            )
            .bind(project_id)
            .bind(id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .flatten()
        } else if let Some(n) = body.name.as_deref() {
            sqlx::query_scalar(
                "SELECT connection_secret FROM project_database_config WHERE project_id=$1 AND LOWER(name)=LOWER($2)",
            )
            .bind(project_id)
            .bind(n)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .flatten()
        } else {
            sqlx::query_scalar(
                "SELECT connection_secret FROM project_database_config WHERE project_id=$1 ORDER BY is_primary DESC, LOWER(name) LIMIT 1",
            )
            .bind(project_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .flatten()
        };

        let from_secret = saved
            .and_then(|b| String::from_utf8(b).ok())
            .filter(|s| !s.trim().is_empty());

        if let Some(s) = from_secret {
            s
        } else {
            let detected: Option<String> = sqlx::query_scalar::<_, Option<String>>(
                r#"SELECT detection_metadata->>'connection_string'
                   FROM project_database_config WHERE project_id=$1
                   ORDER BY is_primary DESC, LOWER(name) LIMIT 1"#,
            )
            .bind(project_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .flatten();
            detected.unwrap_or_else(|| resolve_project_db_url(project_id))
        }
    };

    let engine = body
        .engine
        .unwrap_or_else(|| {
            if url.starts_with("mysql") { "mysql".into() }
            else if url.starts_with("sqlite") { "sqlite".into() }
            else if url.starts_with("jdbc:sqlserver") || {
                let lc = url.to_lowercase();
                lc.contains("server=") && (lc.contains("initial catalog=") || lc.contains("database=") || lc.contains("data source="))
            } { "sqlserver".into() }
            else { "postgres".into() }
        });

    let started = std::time::Instant::now();
    match engine.as_str() {
        "postgres" => {
            // Converte stringhe ADO.NET (Host=...;Port=...) in URL postgres://...
            let pg_url = normalize_pg_connection_string(&url);
            match sqlx::postgres::PgPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(std::time::Duration::from_secs(5))
                .connect(&pg_url)
                .await
            {
                Ok(pool) => {
                    let ver: Result<(String,), _> = sqlx::query_as("SELECT version()")
                        .fetch_one(&pool).await;
                    let count: Result<(i64,), _> = sqlx::query_as(
                        "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema='public'"
                    ).fetch_one(&pool).await;
                    pool.close().await;
                    Ok(Json(json!({
                        "ok": true,
                        "engine": "postgres",
                        "server_version": ver.ok().map(|(v,)| v),
                        "table_count": count.ok().map(|(c,)| c),
                        "latency_ms": started.elapsed().as_millis() as u64,
                    })))
                }
                Err(e) => Ok(Json(json!({
                    "ok": false,
                    "engine": "postgres",
                    "error": e.to_string(),
                }))),
            }
        }
        "mysql" => {
            Ok(Json(json!({
                "ok": false,
                "engine": "mysql",
                "error": "Driver MySQL non abilitato in mcp-core; configurare sqlx feature 'mysql' per abilitarlo.",
            })))
        }
        "sqlite" => {
            Ok(Json(json!({
                "ok": false,
                "engine": "sqlite",
                "error": "Driver SQLite non abilitato in mcp-core; configurare sqlx feature 'sqlite' per abilitarlo.",
            })))
        }
        "sqlserver" => {
            match test_sqlserver_connection(&url).await {
                Ok((version, table_count)) => Ok(Json(json!({
                    "ok": true,
                    "engine": "sqlserver",
                    "server_version": version,
                    "table_count": table_count,
                    "latency_ms": started.elapsed().as_millis() as u64,
                }))),
                Err(e) => {
                    let msg = e.to_string();
                    // Aggiunge un suggerimento contestuale per gli errori SQL Server più comuni
                    let hint = if msg.contains("4060") || msg.contains("non è possibile aprire il database") || msg.contains("Cannot open database") {
                        Some("Il database esiste ma l'utente non ha accesso: verifica che l'account SQL abbia il permesso 'db_datareader' (o superiore) sul database specificato.")
                    } else if msg.contains("18456") || msg.contains("L'accesso non è riuscito") || msg.contains("Login failed") {
                        Some("Credenziali non valide: verifica utente e password nella connection string.")
                    } else if msg.contains("Impossibile raggiungere") || msg.contains("Connection refused") || msg.contains("timed out") {
                        Some("Server non raggiungibile: verifica host, porta e che il servizio SQL Server sia in ascolto.")
                    } else {
                        None
                    };
                    Ok(Json(json!({
                        "ok": false,
                        "engine": "sqlserver",
                        "error": msg,
                        "hint": hint,
                    })))
                }
            }
        }
        other => Ok(Json(json!({
            "ok": false,
            "engine": other,
            "error": format!("Engine non supportato: {other}"),
        }))),
    }
}

/// Testa la connessione a SQL Server usando tiberius (driver TDS nativo).
/// Accetta connection string in formato ADO.NET oppure JDBC-like:
///   - ADO.NET: `Server=host,port;Database=db;User Id=user;Password=pwd;...`
///   - JDBC:    `jdbc:sqlserver://host:port;databaseName=db;user=user;password=pwd`
async fn test_sqlserver_connection(conn_str: &str) -> anyhow::Result<(String, i64)> {
    use tiberius::{Client, Config};
    use tokio::net::TcpStream;
    use tokio_util::compat::TokioAsyncWriteCompatExt;

    // Usa il parser ADO.NET manuale per massima compatibilita'.
    // tiberius from_ado_string non riconosce "User Id" (chiave ADO.NET ufficiale .NET/C#).
    let config = if conn_str.trim_start().to_lowercase().starts_with("jdbc:") {
        Config::from_jdbc_string(conn_str)
            .map_err(|e| anyhow::anyhow!("Connection string JDBC non valida: {e}"))?
    } else {
        build_sqlserver_config(conn_str)?
    };

    let addr = config.get_addr();
    let tcp = TcpStream::connect(&addr)
        .await
        .map_err(|e| anyhow::anyhow!("Impossibile raggiungere il server ({addr}): {e}"))?;
    tcp.set_nodelay(true)?;

    let mut client = Client::connect(config, tcp.compat_write())
        .await
        .map_err(|e| {
            let raw = e.to_string();
            if let Some(start) = raw.find("Token error: '") {
                let inner = &raw[start + "Token error: '".len()..];
                let clean = if let Some(p) = inner.find("' on server") {
                    &inner[..p]
                } else if let Some(p) = inner.rfind('\'') {
                    &inner[..p]
                } else {
                    inner.trim_end_matches('\'')
                };
                anyhow::anyhow!("{}", clean)
            } else if raw.to_lowercase().contains("tls") || raw.to_lowercase().contains("certificate") {
                anyhow::anyhow!("Errore TLS/certificato: {raw}")
            } else {
                anyhow::anyhow!("Login SQL Server fallito: {raw}")
            }
        })?;

    // Versione server
    let version: String = client
        .query("SELECT @@VERSION", &[])
        .await
        .map_err(|e| anyhow::anyhow!("Query @@VERSION fallita: {e}"))?
        .into_row()
        .await?
        .and_then(|r| r.get::<&str, usize>(0).map(String::from))
        .unwrap_or_else(|| "sconosciuta".into());

    // Numero tabelle nel database corrente
    let table_count: i64 = client
        .query(
            "SELECT CAST(COUNT(*) AS BIGINT) FROM INFORMATION_SCHEMA.TABLES WHERE TABLE_TYPE='BASE TABLE'",
            &[],
        )
        .await
        .map_err(|e| anyhow::anyhow!("Query tabelle fallita: {e}"))?
        .into_row()
        .await?
        .and_then(|r| r.get::<i64, usize>(0))
        .unwrap_or(0);

    Ok((version, table_count))
}

/// Parser ADO.NET manuale per costruire un `Config` tiberius.
///
/// Gestisce tutte le varianti di chiave usate in .NET:
///   - `Server` / `Data Source`                -> host + porta
///   - `Database` / `Initial Catalog`          -> nome database
///   - `User Id` / `User ID` / `UID` / `User`  -> username SQL
///   - `Password` / `PWD`                      -> password
///   - `Encrypt`                               -> livello crittografia
///   - `TrustServerCertificate`                -> trust certificato self-signed
fn build_sqlserver_config(conn_str: &str) -> anyhow::Result<tiberius::Config> {
    use std::collections::HashMap;
    use tiberius::{AuthMethod, Config, EncryptionLevel};

    // Tokenizza "Key=Value;" ignorando segmenti vuoti (es. ; finale)
    let mut params: HashMap<String, String> = HashMap::new();
    for part in conn_str.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(eq_pos) = part.find('=') {
            // Normalizza chiave: lowercase, spazi multipli -> singolo spazio
            let key = part[..eq_pos]
                .trim()
                .to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let val = part[eq_pos + 1..].trim().to_string();
            params.insert(key, val);
        }
    }

    let mut config = Config::new();

    // ── Host + porta ─────────────────────────────────────────────────────────
    let server = params
        .get("server")
        .or_else(|| params.get("data source"))
        .map(|s| s.as_str())
        .unwrap_or("localhost");

    // Formati: "host,porta" | "tcp:host,porta" | "host\istanza" | "host"
    let server_clean = server.trim_start_matches("tcp:");
    let (host, port) = if let Some(comma) = server_clean.find(',') {
        let h = &server_clean[..comma];
        let p: u16 = server_clean[comma + 1..].trim().parse().unwrap_or(1433);
        (h, p)
    } else if let Some(bs) = server_clean.find('\\') {
        (&server_clean[..bs], 1433u16)
    } else {
        (server_clean, 1433u16)
    };

    config.host(host);
    config.port(port);

    // ── Database ─────────────────────────────────────────────────────────────
    if let Some(db) = params
        .get("database")
        .or_else(|| params.get("initial catalog"))
    {
        config.database(db.as_str());
    }

    // ── Autenticazione ───────────────────────────────────────────────────────
    // Chiavi normalizzate (lowercase, spazio singolo):
    //   "user id" -> "User Id" / "User ID" (ADO.NET ufficiale .NET/C#)
    //   "uid"     -> abbreviazione
    //   "user"    -> variante breve
    let user = params
        .get("user id")
        .or_else(|| params.get("uid"))
        .or_else(|| params.get("user"))
        .cloned();
    let pwd = params
        .get("password")
        .or_else(|| params.get("pwd"))
        .cloned();

    match (user, pwd) {
        (Some(u), Some(p)) => {
            config.authentication(AuthMethod::sql_server(u, p));
        }
        (Some(u), None) => {
            config.authentication(AuthMethod::sql_server(u, String::new()));
        }
        _ => {
            // Nessuna credenziale SQL: Windows Auth (non disponibile su Linux senza Kerberos)
        }
    }

    // ── Crittografia ─────────────────────────────────────────────────────────
    let encrypt = params.get("encrypt").map(|s| s.to_lowercase());
    match encrypt.as_deref() {
        Some("false") | Some("no") | Some("0") | Some("optional") => {
            config.encryption(EncryptionLevel::Off);
        }
        Some("true") | Some("yes") | Some("1") | Some("mandatory") | Some("strict") => {
            config.encryption(EncryptionLevel::Required);
        }
        _ => {}
    }

    // ── TrustServerCertificate ───────────────────────────────────────────────
    let trust = params
        .get("trustservercertificate")
        .or_else(|| params.get("trust server certificate"))
        .map(|s| s.to_lowercase());
    if matches!(trust.as_deref(), Some("true") | Some("yes") | Some("1")) {
        config.trust_cert();
    }

    Ok(config)
}

// ── POST /api/projects/:id/db/query ──────────────────────────────────────────
//
// Esegue una query SQL ad-hoc sul DB applicativo del progetto, invocata dal
// pannello SQL del frontend (componente `sql-query-panel.tsx`). La logica
// vera vive in `crate::project_db::exec::execute_query`, condivisa con il tool
// MCP `nexus_db_query` (regola H: niente duplicazione).
//
// Sicurezza: la connessione viene risolta da `project_database_config` con
// guard-rail anti-Nexus (vedi `crate::project_db::exec::resolve_project_conn`).
// L'agente del frontend non puo' passare una connection string arbitraria.
//
// Dopo l'esecuzione emette un evento dispatcher `ProjectEvent::DbQueryRun`
// per far ri-renderizzare lo store frontend (RecentQueriesSection ecc.).

pub async fn execute_project_db_query(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
    Json(body): Json<ExecuteQueryBody>,
) -> ApiResult {
    let sql = body.sql.trim().to_string();
    if sql.is_empty() {
        return Err(api_err(
            StatusCode::BAD_REQUEST,
            "Campo 'sql' obbligatorio (stringa non vuota).",
        ));
    }

    // Normalizza params: array JSON -> Vec<Option<String>> (NULL -> None;
    // ogni altro valore -> String). Stesso contratto del tool agente.
    let params: Vec<Option<String>> = body
        .params
        .iter()
        .map(|v| match v {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        })
        .collect();

    let outcome = execute_query(
        &state.db,
        project_id,
        &sql,
        &params,
        body.max_rows,
        body.connection.as_deref(),
    )
    .await
    .map_err(|e| match e {
        QueryExecError::ConnectionError(m) => api_err(StatusCode::BAD_REQUEST, m),
        QueryExecError::Timeout => api_err(StatusCode::REQUEST_TIMEOUT, e.message()),
        QueryExecError::Sql(_) => api_err(StatusCode::UNPROCESSABLE_ENTITY, e.message()),
    })?;

    // Emit dispatcher event: il frontend (store project-dispatcher) lo
    // intercetta e aggiorna RecentQueriesSection nel pannello DB esistente.
    let rows_for_event: i64 = match outcome.mode {
        "read" => outcome.row_count as i64,
        _ => outcome.rows_affected.unwrap_or(0) as i64,
    };
    let _ = nexus_events::dispatcher::emit(
        &state.project_channels,
        project_id,
        nexus_events::ProjectEvent::DbQueryRun {
            query_id: None,
            duration_ms: outcome.duration_ms as i64,
            rows: rows_for_event,
            statement_kind: outcome.statement_kind.clone(),
        },
    );

    // Archiviazione DDL automatica (best effort): nota KB + file migration
    // versionato. La logica scatta SOLO per statement_kind="ddl" e per
    // esecuzioni riuscite. Vedi `crate::project_db::exec::archive_ddl`.
    let archive = archive_ddl(&state.db, project_id, &sql, &outcome).await;
    if let Some(ref archived) = archive {
        // Emit evento KnowledgeNoteCreated cosi' il pannello KB si rinfresca.
        let _ = nexus_events::dispatcher::emit(
            &state.project_channels,
            project_id,
            nexus_events::ProjectEvent::KnowledgeNoteCreated {
                note_id: archived.note_id,
                title: format!("DDL archiviata · {}", archived
                    .migration_filename
                    .clone()
                    .unwrap_or_else(|| "(senza file)".into())),
                intent: Some("database_migration".to_string()),
            },
        );
    }

    // Costruisce il payload di risposta. Stesso schema usato dal tool agente
    // (serializzato da `crate::project_db::exec::outcome_to_json`), arricchito
    // con il blocco `archived_ddl` quando rilevante.
    let mut payload = crate::project_db::exec::outcome_to_json(&outcome);
    if let Some(archived) = archive {
        if let Value::Object(ref mut map) = payload {
            map.insert(
                "archived_ddl".to_string(),
                json!({
                    "note_id": archived.note_id.to_string(),
                    "migration_filename": archived.migration_filename,
                    "migration_abs_path": archived.migration_abs_path,
                }),
            );
        }
    }
    Ok(Json(payload))
}

