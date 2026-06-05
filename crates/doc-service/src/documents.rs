use axum::{
    body::Body,
    extract::{Extension, Path as AxumPath, State},
    http::{header, StatusCode},
    Json,
};
use nexus_types::{api_error, parse_user_id, ApiError, ApiResult};
use nexus_auth::Claims;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use tokio::fs;
use uuid::Uuid;

use crate::AppState;

/// GET /api/projects/:id/documents
pub async fn list_documents(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    let rows = sqlx::query(
        "SELECT id, project_id, doc_type, title, version, file_path, status, metadata, created_at, updated_at
         FROM project_documents WHERE project_id = $1 ORDER BY doc_type, updated_at DESC",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}")))?;

    let docs: Vec<Value> = rows.iter().map(|r| json!({
        "id": r.get::<Uuid, _>("id").to_string(),
        "project_id": r.get::<Uuid, _>("project_id").to_string(),
        "doc_type": r.get::<String, _>("doc_type"),
        "title": r.get::<String, _>("title"),
        "version": r.get::<String, _>("version"),
        "file_path": r.get::<String, _>("file_path"),
        "status": r.get::<String, _>("status"),
        "metadata": r.get::<Value, _>("metadata"),
        "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
        "updated_at": r.get::<chrono::DateTime<chrono::Utc>, _>("updated_at").to_rfc3339(),
    })).collect();

    Ok(Json(json!({ "documents": docs })))
}

/// GET /api/projects/:id/documents/:doc_id
pub async fn get_document(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((_id, doc_id)): AxumPath<(String, String)>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let document_id = Uuid::parse_str(&doc_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Document id non valido"))?;

    let row = sqlx::query(
        "SELECT id, project_id, doc_type, title, version, file_path, structure_json, status, metadata, created_at, updated_at
         FROM project_documents WHERE id = $1",
    )
    .bind(document_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}")))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Documento non trovato"))?;

    // Punto unico mapping JSON in nexus_types::documents_dto (regola L, S62).
    Ok(Json(nexus_types::documents_dto::document_row_to_json(&row)))
}

/// GET /api/projects/:id/documents/:doc_id/download
pub async fn download_document(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((id, doc_id)): AxumPath<(String, String)>,
) -> Result<axum::response::Response<Body>, ApiError> {
    let _user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let document_id = Uuid::parse_str(&doc_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Document id non valido"))?;

    let row = sqlx::query("SELECT file_path, title FROM project_documents WHERE id = $1 AND project_id = $2")
        .bind(document_id).bind(project_id)
        .fetch_optional(&state.db).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}")))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Documento non trovato"))?;

    let file_path: String = row.get("file_path");

    // Resolve project root path from workspace
    let root_row = sqlx::query("SELECT absolute_path FROM workspaces WHERE project_id = $1 AND is_primary = TRUE")
        .bind(project_id)
        .fetch_optional(&state.db).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}")))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Workspace non trovato"))?;

    let root_path: String = root_row.get("absolute_path");
    let abs_path = std::path::PathBuf::from(&root_path).join(&file_path);

    if !abs_path.exists() {
        return Err(api_error(StatusCode::NOT_FOUND, "File non trovato sul filesystem"));
    }

    let bytes = fs::read(&abs_path).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("Errore lettura: {e}")))?;

    let filename = abs_path.file_name().and_then(|n| n.to_str()).unwrap_or("document.docx");

    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/vnd.openxmlformats-officedocument.wordprocessingml.document")
        .header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{}\"", filename))
        .body(Body::from(bytes))
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("Response error: {e}")))
}

/// GET /api/projects/:id/documents/:doc_id/versions
pub async fn list_versions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((_id, doc_id)): AxumPath<(String, String)>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let document_id = Uuid::parse_str(&doc_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Document id non valido"))?;

    let rows = sqlx::query(
        "SELECT id, document_id, version, file_path, change_summary, changed_sections, created_at
         FROM project_document_versions WHERE document_id = $1 ORDER BY created_at DESC",
    )
    .bind(document_id)
    .fetch_all(&state.db).await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}")))?;

    let versions: Vec<Value> = rows.iter().map(|r| json!({
        "id": r.get::<Uuid, _>("id").to_string(),
        "version": r.get::<String, _>("version"),
        "file_path": r.get::<String, _>("file_path"),
        "change_summary": r.get::<Option<String>, _>("change_summary"),
        "changed_sections": r.get::<Option<Vec<String>>, _>("changed_sections"),
        "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
    })).collect();

    Ok(Json(json!({ "versions": versions })))
}

/// DELETE /api/projects/:id/documents/:doc_id
pub async fn delete_document(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((id, doc_id)): AxumPath<(String, String)>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let document_id = Uuid::parse_str(&doc_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Document id non valido"))?;

    let row = sqlx::query("SELECT file_path, qdrant_point_ids FROM project_documents WHERE id = $1 AND project_id = $2")
        .bind(document_id).bind(project_id)
        .fetch_optional(&state.db).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}")))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Documento non trovato"))?;

    let file_path: String = row.get("file_path");
    let qdrant_point_ids: Vec<String> = row.get("qdrant_point_ids");

    // Delete from DB
    sqlx::query("DELETE FROM project_documents WHERE id = $1")
        .bind(document_id).execute(&state.db).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}")))?;

    // Delete file
    let root_row = sqlx::query("SELECT absolute_path FROM workspaces WHERE project_id = $1 AND is_primary = TRUE")
        .bind(project_id).fetch_optional(&state.db).await.ok().flatten();

    if let Some(root_row) = root_row {
        let root_path: String = root_row.get("absolute_path");
        let abs_path = std::path::PathBuf::from(&root_path).join(&file_path);
        let _ = fs::remove_file(&abs_path).await;
    }

    // Delete Qdrant points
    if !qdrant_point_ids.is_empty() {
        let _ = crate::vector::delete_doc_points(&state.qdrant_url, &qdrant_point_ids).await;
    }

    Ok(Json(json!({ "deleted": true })))
}

/// POST /api/documents/generate
#[derive(Debug, Deserialize)]
pub struct GenerateDocRequest {
    pub project_id: String,
    pub doc_type: String,
    pub content_json: Value,
    pub title: Option<String>,
    pub standard: Option<String>,
}

pub async fn generate_document(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<GenerateDocRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let pid = Uuid::parse_str(&req.project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    let standard = req.standard.as_deref().unwrap_or("ieee830");

    // Get project info
    let root_row = sqlx::query("SELECT w.absolute_path, p.name FROM workspaces w JOIN projects p ON p.id = w.project_id WHERE w.project_id = $1 AND w.is_primary = TRUE")
        .bind(pid).fetch_optional(&state.db).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("DB: {e}")))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Progetto non trovato"))?;

    let root_path: String = root_row.get("absolute_path");
    let project_name: String = root_row.get("name");

    // Determine version
    let existing = sqlx::query("SELECT version FROM project_documents WHERE project_id = $1 AND doc_type = $2 ORDER BY created_at DESC LIMIT 1")
        .bind(pid).bind(&req.doc_type).fetch_optional(&state.db).await.ok().flatten();

    let version = match existing {
        Some(r) => {
            let v: String = r.try_get("version").unwrap_or_else(|_| "1.0.0".to_string());
            bump_version(&v, "minor")
        }
        None => "1.0.0".to_string(),
    };

    let slug = req.doc_type.replace('_', "-");
    let relative_path = format!("docs/{}-v{}.docx", slug, version);
    let abs_output = format!("{}/{}", root_path, relative_path);
    let content_str = serde_json::to_string(&req.content_json).unwrap_or_default();

    let final_title = req.title.as_deref().filter(|t| !t.is_empty())
        .map(String::from)
        .unwrap_or_else(|| slug.replace('-', " "));

    // Call brain REST for document generation
    let brain_rest_url = std::env::var("NEURAL_CORE_REST_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap_or_default();

    let resp = client.post(format!("{brain_rest_url}/generate-document"))
        .json(&json!({
            "doc_type": req.doc_type,
            "content_json": content_str,
            "output_path": abs_output,
            "standard": standard,
            "title": final_title,
            "project_name": project_name,
        }))
        .send().await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("Brain: {e}")))?;

    let result: Value = resp.json().await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("Brain response: {e}")))?;

    if let Some(err) = result.get("error").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        return Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, err));
    }

    let page_count = result.get("page_count").and_then(Value::as_i64).unwrap_or(0);
    let section_count = result.get("section_count").and_then(Value::as_i64).unwrap_or(0);

    // Save to DB
    let doc_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO project_documents (id, project_id, doc_type, title, version, file_path, structure_json, status, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, 'draft', $8)"
    )
    .bind(doc_id).bind(pid).bind(&req.doc_type).bind(&final_title)
    .bind(&version).bind(&relative_path).bind(&req.content_json).bind(user_id)
    .execute(&state.db).await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("DB: {e}")))?;

    // Vectorize in background
    let db2 = state.db.clone();
    let qdrant_url = state.qdrant_url.clone();
    let neural_url2 = state.neural_url.clone();
    let content2 = req.content_json.clone();
    let doc_type2 = req.doc_type.clone();
    let version2 = version.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::vector::vectorize_document(&db2, &qdrant_url, &neural_url2, pid, doc_id, &doc_type2, &version2, &content2).await {
            tracing::warn!("Vettorializzazione fallita: {e}");
        }
    });

    Ok(Json(json!({
        "ok": true,
        "document_id": doc_id.to_string(),
        "file_path": relative_path,
        "title": final_title,
        "version": version,
        "page_count": page_count,
        "section_count": section_count,
    })))
}

/// POST /api/documents/search
#[derive(Debug, Deserialize)]
pub struct SearchDocRequest {
    pub project_id: String,
    pub query: String,
    pub doc_type: Option<String>,
    pub limit: Option<usize>,
}

pub async fn search_documents(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    Json(req): Json<SearchDocRequest>,
) -> ApiResult {
    let pid = Uuid::parse_str(&req.project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let limit = req.limit.unwrap_or(5);

    // Embed query via brain REST
    let brain_rest_url = std::env::var("NEURAL_CORE_REST_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());

    let client = reqwest::Client::new();
    let resp = client.post(format!("{brain_rest_url}/embed"))
        .json(&json!({ "text": req.query }))
        .send().await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("Embedding: {e}")))?;

    let embed_result: Value = resp.json().await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("Embed response: {e}")))?;

    let vector: Vec<f32> = embed_result.get("vector")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_f64).map(|v| v as f32).collect())
        .unwrap_or_default();

    if vector.is_empty() {
        return Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, "Embedding vuoto"));
    }

    let results = crate::vector::search_doc_points(
        &state.qdrant_url, &vector, pid, req.doc_type.as_deref(), limit,
    ).await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("Qdrant: {e}")))?;

    Ok(Json(json!({ "results": results, "query": req.query })))
}

fn bump_version(current: &str, bump_type: &str) -> String {
    let parts: Vec<u32> = current.split('.').filter_map(|p| p.parse().ok()).collect();
    let (major, minor, patch) = (
        parts.first().copied().unwrap_or(1),
        parts.get(1).copied().unwrap_or(0),
        parts.get(2).copied().unwrap_or(0),
    );
    match bump_type {
        "major" => format!("{}.0.0", major + 1),
        "minor" => format!("{}.{}.0", major, minor + 1),
        _ => format!("{}.{}.{}", major, minor, patch + 1),
    }
}
