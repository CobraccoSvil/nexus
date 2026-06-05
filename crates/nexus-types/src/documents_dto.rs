//! Helper JSON per la tabella `project_documents`.
//!
//! Punto unico (regola L / ADR 0026): prima il mapping `row -> JSON` era
//! duplicato fra `crates/doc-service/src/documents.rs` (riga 69+) e
//! `crates/mcp-core/src/documents.rs` (riga 168+) — cluster 31L jscpd.

use serde_json::{json, Value};
use sqlx::{postgres::PgRow, Row};
use uuid::Uuid;

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
