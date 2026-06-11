//! Migrazioni del database del progetto e override DDL.
//!
//! Route:
//!   GET  /api/projects/:id/db/migrations          -> list_project_migrations
//!   POST /api/projects/:id/db/migrations/apply    -> apply_project_migrations
//!   POST /api/projects/:id/db/migrations/rollback -> rollback_project_migration
//!   POST /api/projects/:id/db/override-request    -> request_ddl_override

use axum::{
    extract::{Extension, Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use super::shared::{api_err, ApiResult};
use crate::project_db::exec::{
    archive_ddl, execute_query, open_pool, resolve_project_conn, QueryExecError,
};
use crate::{auth::Claims, AppState};

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

    let migrations: Vec<serde_json::Value> = rows
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

    // Risolve la connessione decifrando `connection_secret` dalla stessa
    // funzione usata dal pannello SQL (regola H: niente risoluzione divergente).
    // `None` -> connessione is_primary, comportamento storico delle migrazioni.
    let db_url = resolve_project_conn(&state.db, project_id, None)
        .await
        .map_err(|e| {
            api_err(
                StatusCode::BAD_GATEWAY,
                format!("Connessione DB progetto fallita: {e}"),
            )
        })?;

    let project_pool = open_pool(&db_url).await.map_err(|e| {
        api_err(
            StatusCode::BAD_GATEWAY,
            format!("Connessione DB progetto fallita: {e}"),
        )
    })?;

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
        None => {
            return Ok(Json(
                json!({ "ok": false, "error": "Nessuna migration applicata da rollbackare" }),
            ))
        }
        Some(r) => r,
    };

    let migration_id: Uuid = row.get("id");
    let filename: String = row.get("filename");
    let rollback_sql: Option<String> = row.try_get("rollback_sql").unwrap_or(None);

    if let Some(sql) = rollback_sql {
        if !sql.trim().is_empty() {
            let db_url = resolve_project_conn(&state.db, project_id, None)
                .await
                .map_err(|e| {
                    api_err(
                        StatusCode::BAD_GATEWAY,
                        format!("Connessione DB progetto fallita: {e}"),
                    )
                })?;
            let project_pool = open_pool(&db_url).await.map_err(|e| {
                api_err(
                    StatusCode::BAD_GATEWAY,
                    format!("Connessione DB progetto fallita: {e}"),
                )
            })?;

            sqlx::raw_sql(&sql)
                .execute(&project_pool)
                .await
                .map_err(|e| {
                    api_err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Rollback SQL fallito: {e}"),
                    )
                })?;
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
        return Err(api_err(
            StatusCode::BAD_REQUEST,
            "reason deve avere almeno 10 caratteri",
        ));
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
    let filename = format!(
        "override_{}_{}.sql",
        now.format("%Y%m%d_%H%M%S"),
        &checksum[..8]
    );

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

    // Esegui il DDL via la STESSA pipeline del pannello SQL (regola H): la
    // connessione viene risolta decifrando `connection_secret` (resolve_project_conn
    // dentro execute_query). Cosi' l'override usa identica risoluzione del client
    // SQL e di "Testa connessione", invece di una risoluzione divergente che
    // falliva con "impossibile connettersi al DB del progetto". In piu',
    // archive_ddl registra automaticamente nota KB + file migration versionato.
    // L'approvazione UI e' gia' avvenuta nel front-end (OverrideConfirmDialog).
    match execute_query(&state.db, project_id, &body.sql, &[], None, None).await {
        Ok(outcome) => {
            let _ = archive_ddl(&state.db, project_id, &body.sql, &outcome, None).await;
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
        Err(QueryExecError::ConnectionError(msg)) => {
            // Lascia il record in pending_override — l'admin può ritentare.
            tracing::warn!(
                %project_id,
                error = %msg,
                "Override DDL: connessione DB progetto non risolvibile"
            );
            return Ok(Json(json!({
                "ok": false,
                "pending_override_id": migration_id.to_string(),
                "warning": format!("Impossibile connettersi al DB: {msg}. Il record è salvato come pending_override."),
            })));
        }
        Err(e) => {
            let msg = e.message();
            let _ = sqlx::query(
                "UPDATE project_migration_history SET status='failed', error_message=$2 WHERE id=$1"
            )
            .bind(migration_id)
            .bind(&msg)
            .execute(&state.db)
            .await;
            return Err(api_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("DDL override fallito: {msg}"),
            ));
        }
    }

    Ok(Json(json!({
        "ok": true,
        "migration_id": migration_id.to_string(),
        "filename": filename,
    })))
}
