//! Esecuzione query del client SQL e import schema del database del progetto.
//!
//! Route:
//!   POST /api/projects/:id/db/query         -> execute_project_db_query
//!   POST /api/projects/:id/db/import-schema -> import_project_db_schema
//!
//! La logica vera vive in `crate::project_db::exec` (regola H: stessa pipeline
//! del tool MCP `nexus_db_query`, con archive_ddl automatico).

use axum::{
    extract::{Extension, Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::shared::{api_err, ApiError, ApiResult};
use crate::project_db::exec::{archive_ddl, execute_query, QueryExecError};
use crate::{auth::Claims, AppState};

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

#[derive(Debug, Deserialize)]
pub struct ImportSchemaBody {
    /// Percorso del file SQL relativo alla root del progetto (o assoluto sotto
    /// la root). Se omesso, l'endpoint cerca candidati comuni e, se ne trova
    /// piu' di uno, ritorna la lista perche' il chiamante scelga.
    #[serde(default)]
    pub file_path: Option<String>,
    /// Connessione DB del progetto su cui eseguire (default: is_primary).
    #[serde(default)]
    pub connection: Option<String>,
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
    // esecuzioni riuscite. Multi-DB: passa body.connection cosi' le
    // migration di non-primary finiscono in nexus_migrations/<conn>/
    // separate. Vedi `crate::project_db::exec::archive_ddl`.
    let archive = archive_ddl(
        &state.db,
        project_id,
        &sql,
        &outcome,
        body.connection.as_deref(),
    )
    .await;
    if let Some(ref archived) = archive {
        // Emit evento KnowledgeNoteCreated cosi' il pannello KB si rinfresca.
        let _ = nexus_events::dispatcher::emit(
            &state.project_channels,
            project_id,
            nexus_events::ProjectEvent::KnowledgeNoteCreated {
                note_id: archived.note_id,
                title: format!(
                    "DDL archiviata · {}",
                    archived
                        .migration_filename
                        .clone()
                        .unwrap_or_else(|| "(senza file)".into())
                ),
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

// -- POST /api/projects/:id/db/import-schema --------------------------------
// Importa lo schema da un file SQL del progetto ed esegue il contenuto via
// execute_query (regola H: stessa pipeline del pannello SQL, con archive_ddl
// automatico). Se file_path manca, cerca candidati comuni sotto
// repository_root_path; se ne trova piu di uno (ambiguo) ritorna la lista
// senza eseguire nulla, cosi il chiamante (UI o agente) sceglie.

const SCHEMA_FILE_CANDIDATES: &[&str] = &[
    "backend/db_schema.sql",
    "db_schema.sql",
    "schema.sql",
    "db/schema.sql",
    "database/schema.sql",
    "sql/schema.sql",
];

async fn project_root_path(db: &sqlx::PgPool, project_id: Uuid) -> Result<String, ApiError> {
    let root: Option<String> =
        sqlx::query_scalar("SELECT repository_root_path FROM projects WHERE id=$1")
            .bind(project_id)
            .fetch_optional(db)
            .await
            .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .flatten();
    root.map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            api_err(
                StatusCode::BAD_REQUEST,
                "repository_root_path non configurato per il progetto.",
            )
        })
}

pub async fn discover_schema_candidates(root: &str) -> Vec<String> {
    let root_path = std::path::Path::new(root);
    let mut found: Vec<String> = Vec::new();

    for cand in SCHEMA_FILE_CANDIDATES {
        if root_path.join(cand).is_file() {
            found.push((*cand).to_string());
        }
    }

    for dir in ["migrations", "db/migrations", "sql/migrations"] {
        let abs = root_path.join(dir);
        if let Ok(mut rd) = tokio::fs::read_dir(&abs).await {
            let mut names: Vec<String> = Vec::new();
            while let Ok(Some(entry)) = rd.next_entry().await {
                if let Some(name) = entry.file_name().to_str() {
                    if name.to_lowercase().ends_with(".sql") {
                        names.push(format!("{}/{}", dir, name));
                    }
                }
            }
            names.sort();
            found.extend(names);
        }
    }

    found.sort();
    found.dedup();
    found
}

pub async fn read_schema_file(root: &str, file_path: &str) -> Result<(String, String), String> {
    let root_path = std::path::Path::new(root);
    let candidate = std::path::Path::new(file_path);
    let abs = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root_path.join(candidate)
    };

    let canon = tokio::fs::canonicalize(&abs)
        .await
        .map_err(|e| format!("file non trovato: {} ({})", abs.display(), e))?;
    let root_canon = tokio::fs::canonicalize(root_path)
        .await
        .map_err(|e| format!("root progetto non accessibile: {}", e))?;
    if !canon.starts_with(&root_canon) {
        return Err("il file e fuori dalla root del progetto (path non ammesso).".to_string());
    }
    if !canon.is_file() {
        return Err(format!("il percorso non e un file: {}", canon.display()));
    }

    let content = tokio::fs::read_to_string(&canon)
        .await
        .map_err(|e| format!("lettura file fallita: {}", e))?;
    let rel = canon
        .strip_prefix(&root_canon)
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_else(|_| file_path.to_string());
    Ok((rel, content))
}

async fn count_public_tables(
    db: &sqlx::PgPool,
    project_id: Uuid,
    connection: Option<&str>,
) -> Option<i64> {
    let outcome = execute_query(
        db,
        project_id,
        "SELECT COUNT(*)::bigint AS n FROM information_schema.tables WHERE table_schema = 'public'",
        &[],
        Some(1),
        connection,
    )
    .await
    .ok()?;
    outcome
        .rows
        .first()
        .and_then(|r| r.get("n"))
        .and_then(Value::as_i64)
}

pub async fn import_project_db_schema(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
    Json(body): Json<ImportSchemaBody>,
) -> ApiResult {
    // Owner check (punto unico, regola L).
    super::shared::ensure_project_owner(&state.db, project_id, &claims).await?;

    let root = project_root_path(&state.db, project_id).await?;
    let connection = body.connection.as_deref();

    let chosen =
        match body
            .file_path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(fp) => fp.to_string(),
            None => {
                let candidates = discover_schema_candidates(&root).await;
                match candidates.len() {
                    0 => return Err(api_err(
                        StatusCode::NOT_FOUND,
                        "Nessun file schema trovato nel progetto. Indica file_path esplicitamente.",
                    )),
                    1 => candidates[0].clone(),
                    _ => {
                        return Ok(Json(json!({
                            "ok": false,
                            "ambiguous": true,
                            "candidates": candidates,
                            "message": "Piu file schema trovati. Specifica file_path.",
                        })));
                    }
                }
            }
        };

    let (rel_file, sql) = read_schema_file(&root, &chosen)
        .await
        .map_err(|e| api_err(StatusCode::BAD_REQUEST, e))?;

    if sql.trim().is_empty() {
        return Err(api_err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Il file schema e vuoto.",
        ));
    }

    let outcome = execute_query(&state.db, project_id, &sql, &[], None, connection)
        .await
        .map_err(|e| match e {
            QueryExecError::ConnectionError(m) => api_err(StatusCode::BAD_GATEWAY, m),
            QueryExecError::Timeout => api_err(StatusCode::REQUEST_TIMEOUT, e.message()),
            QueryExecError::Sql(_) => api_err(StatusCode::UNPROCESSABLE_ENTITY, e.message()),
        })?;

    let archive = archive_ddl(&state.db, project_id, &sql, &outcome, connection).await;
    if let Some(ref archived) = archive {
        let _ = nexus_events::dispatcher::emit(
            &state.project_channels,
            project_id,
            nexus_events::ProjectEvent::KnowledgeNoteCreated {
                note_id: archived.note_id,
                title: format!("Schema importato - {}", rel_file),
                intent: Some("database_migration".to_string()),
            },
        );
    }

    let tables_after = count_public_tables(&state.db, project_id, connection).await;

    Ok(Json(json!({
        "ok": true,
        "file": rel_file,
        "statements_run": outcome.statements_executed,
        "tables_after": tables_after,
        "archived_ddl": archive.as_ref().map(|a| json!({
            "note_id": a.note_id.to_string(),
            "migration_filename": a.migration_filename,
        })),
    })))
}
