//! Helper JSON + query per le tabelle `project_documents` / `project_document_versions`.
//!
//! Punto unico (regola L / ADR 0026): mapping `row -> JSON` e query di lettura,
//! download, versioni e delete erano duplicati fra
//! `crates/doc-service/src/documents.rs` e `crates/mcp-core/src/documents.rs`
//! (cluster E4 jscpd). Entrambi gli handler delegano qui.

use axum::body::Body;
use axum::http::{header, StatusCode};
use serde_json::{json, Value};
use sqlx::{postgres::PgRow, PgPool, Row};
use std::path::PathBuf;
use uuid::Uuid;

use crate::{api_error, ApiError};

/// Converte una row di `project_documents` (campi: id, project_id, doc_type,
/// title, version, file_path, structure_json, status, metadata, created_at,
/// updated_at) nel payload JSON canonico esposto dagli endpoint API.
pub fn document_row_to_json(row: &PgRow) -> Value {
    json!({
        "id": row.get::<Uuid, _>("id").to_string(),
        "project_id": row.get::<Uuid, _>("project_id").to_string(),
        "doc_type": row.get::<String, _>("doc_type"),
        "title": row.get::<String, _>("title"),
        "version": row.get::<String, _>("version"),
        "file_path": row.get::<String, _>("file_path"),
        "structure_json": row.get::<Value, _>("structure_json"),
        "status": row.get::<String, _>("status"),
        "metadata": row.get::<Value, _>("metadata"),
        "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
        "updated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("updated_at").to_rfc3339(),
    })
}

/// Variante di lista (senza `structure_json`) usata da GET /documents.
fn document_summary_row_to_json(row: &PgRow) -> Value {
    json!({
        "id": row.get::<Uuid, _>("id").to_string(),
        "project_id": row.get::<Uuid, _>("project_id").to_string(),
        "doc_type": row.get::<String, _>("doc_type"),
        "title": row.get::<String, _>("title"),
        "version": row.get::<String, _>("version"),
        "file_path": row.get::<String, _>("file_path"),
        "status": row.get::<String, _>("status"),
        "metadata": row.get::<Value, _>("metadata"),
        "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
        "updated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("updated_at").to_rfc3339(),
    })
}

fn version_row_to_json(row: &PgRow) -> Value {
    json!({
        "id": row.get::<Uuid, _>("id").to_string(),
        "version": row.get::<String, _>("version"),
        "file_path": row.get::<String, _>("file_path"),
        "change_summary": row.get::<Option<String>, _>("change_summary"),
        "changed_sections": row.get::<Option<Vec<String>>, _>("changed_sections"),
        "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
    })
}

fn db_error(e: sqlx::Error) -> ApiError {
    api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}"))
}

fn not_found() -> ApiError {
    api_error(StatusCode::NOT_FOUND, "Documento non trovato")
}

/// Parse dell'id documento dai path param (speculare a `parse_project_id`).
pub fn parse_document_id(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_| api_error(StatusCode::BAD_REQUEST, "Document id non valido"))
}

/// Lista documenti di un progetto, gia' mappata in JSON.
pub async fn fetch_project_documents(
    db: &PgPool,
    project_id: Uuid,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT id, project_id, doc_type, title, version, file_path, status, metadata, created_at, updated_at
         FROM project_documents WHERE project_id = $1 ORDER BY doc_type, updated_at DESC",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .map_err(db_error)?;

    Ok(rows.iter().map(document_summary_row_to_json).collect())
}

/// Row completa di un documento (404 se assente).
pub async fn fetch_document_row(db: &PgPool, document_id: Uuid) -> Result<PgRow, ApiError> {
    sqlx::query(
        "SELECT id, project_id, doc_type, title, version, file_path, structure_json, status, metadata, created_at, updated_at
         FROM project_documents WHERE id = $1",
    )
    .bind(document_id)
    .fetch_optional(db)
    .await
    .map_err(db_error)?
    .ok_or_else(not_found)
}

/// `file_path` relativo di un documento vincolato al progetto (prologo download).
pub async fn fetch_document_file_path(
    db: &PgPool,
    document_id: Uuid,
    project_id: Uuid,
) -> Result<String, ApiError> {
    let row =
        sqlx::query("SELECT file_path FROM project_documents WHERE id = $1 AND project_id = $2")
            .bind(document_id)
            .bind(project_id)
            .fetch_optional(db)
            .await
            .map_err(db_error)?
            .ok_or_else(not_found)?;

    Ok(row.get("file_path"))
}

/// Versioni di un documento, gia' mappate in JSON.
pub async fn fetch_versions(db: &PgPool, document_id: Uuid) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT id, document_id, version, file_path, change_summary, changed_sections, created_at
         FROM project_document_versions WHERE document_id = $1 ORDER BY created_at DESC",
    )
    .bind(document_id)
    .fetch_all(db)
    .await
    .map_err(db_error)?;

    Ok(rows.iter().map(version_row_to_json).collect())
}

/// Parte DB della cancellazione: legge i riferimenti e rimuove la riga.
/// Ritorna `(file_path, qdrant_point_ids)`; la pulizia di filesystem e store
/// vettoriale resta al chiamante (backend diversi nei due servizi).
pub async fn delete_document_db(
    db: &PgPool,
    document_id: Uuid,
    project_id: Uuid,
) -> Result<(String, Vec<String>), ApiError> {
    let row = sqlx::query(
        "SELECT file_path, qdrant_point_ids FROM project_documents WHERE id = $1 AND project_id = $2",
    )
    .bind(document_id)
    .bind(project_id)
    .fetch_optional(db)
    .await
    .map_err(db_error)?
    .ok_or_else(not_found)?;

    let file_path: String = row.get("file_path");
    let qdrant_point_ids: Vec<String> = row.get("qdrant_point_ids");

    sqlx::query("DELETE FROM project_documents WHERE id = $1")
        .bind(document_id)
        .execute(db)
        .await
        .map_err(db_error)?;

    Ok((file_path, qdrant_point_ids))
}

/// Risposta HTTP di download (attachment .docx) per un documento generato.
pub fn docx_attachment_response(
    abs_path: &std::path::Path,
    bytes: Vec<u8>,
) -> Result<axum::response::Response<Body>, ApiError> {
    let filename = abs_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document.docx");

    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        )
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename),
        )
        .body(Body::from(bytes))
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("Response error: {e}")))
}

/// Root primaria del workspace di un progetto (tabella `workspaces`).
pub async fn resolve_workspace_root(db: &PgPool, project_id: Uuid) -> Result<PathBuf, ApiError> {
    let row = sqlx::query(
        "SELECT absolute_path FROM workspaces WHERE project_id = $1 AND is_primary = TRUE",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
    .map_err(db_error)?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Workspace non trovato"))?;

    let root: String = row.get("absolute_path");
    Ok(PathBuf::from(root))
}
