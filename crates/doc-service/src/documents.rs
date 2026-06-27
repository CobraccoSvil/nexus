use axum::{
    body::Body,
    extract::{Extension, Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use nexus_types::documents_dto::{
    delete_document_db, document_row_to_json, docx_attachment_response, fetch_document_file_path,
    fetch_document_row, fetch_project_documents, fetch_versions, parse_document_id,
    resolve_workspace_root,
};
use nexus_types::{api_error, parse_project_id, parse_user_id, ApiError, ApiResult};
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
    let project_id = parse_project_id(&id)?;

    // Query + mapping nel punto unico documents_dto (regola L, cluster E4).
    let docs = fetch_project_documents(&state.db, project_id).await?;

    Ok(Json(json!({ "documents": docs })))
}

/// GET /api/projects/:id/documents/:doc_id
pub async fn get_document(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((_id, doc_id)): AxumPath<(String, String)>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let document_id = parse_document_id(&doc_id)?;

    // Punto unico query + mapping JSON in nexus_types::documents_dto (regola L, S62).
    let row = fetch_document_row(&state.db, document_id).await?;
    Ok(Json(document_row_to_json(&row)))
}

/// GET /api/projects/:id/documents/:doc_id/versions
pub async fn list_versions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((_id, doc_id)): AxumPath<(String, String)>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let versions = fetch_versions(&state.db, parse_document_id(&doc_id)?).await?;

    Ok(Json(json!({ "versions": versions })))
}

/// GET /api/projects/:id/documents/:doc_id/download
pub async fn download_document(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((id, doc_id)): AxumPath<(String, String)>,
) -> Result<axum::response::Response<Body>, ApiError> {
    let _user_id = parse_user_id(&claims)?;
    let project_id = parse_project_id(&id)?;
    let document_id = parse_document_id(&doc_id)?;

    let file_path = fetch_document_file_path(&state.db, document_id, project_id).await?;

    // Resolve project root path from workspace
    let root_path = resolve_workspace_root(&state.db, project_id).await?;
    let abs_path = root_path.join(&file_path);

    if !abs_path.exists() {
        return Err(api_error(StatusCode::NOT_FOUND, "File non trovato sul filesystem"));
    }

    let bytes = fs::read(&abs_path).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("Errore lettura: {e}")))?;

    docx_attachment_response(&abs_path, bytes)
}

/// DELETE /api/projects/:id/documents/:doc_id
pub async fn delete_document(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((id, doc_id)): AxumPath<(String, String)>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let project_id = parse_project_id(&id)?;
    let document_id = parse_document_id(&doc_id)?;

    // Fetch riferimenti + DELETE riga nel punto unico documents_dto.
    let (file_path, qdrant_point_ids) =
        delete_document_db(&state.db, document_id, project_id).await?;

    // Delete file (tollerante: workspace assente non blocca la cancellazione)
    if let Ok(root_path) = resolve_workspace_root(&state.db, project_id).await {
        let abs_path = root_path.join(&file_path);
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
    let pid = parse_project_id(&req.project_id)?;

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

    // Vectorize in background (embedding via mcp-core ONNX, punto unico)
    let db2 = state.db.clone();
    let qdrant_url = state.qdrant_url.clone();
    let mcp_core_url2 = state.mcp_core_url.clone();
    let content2 = req.content_json.clone();
    let doc_type2 = req.doc_type.clone();
    let version2 = version.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::vector::vectorize_document(&db2, &qdrant_url, &mcp_core_url2, pid, doc_id, &doc_type2, &version2, &content2).await {
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
    let pid = parse_project_id(&req.project_id)?;
    let limit = req.limit.unwrap_or(5);

    // Embed query via mcp-core ONNX (POST /api/embed) — punto unico embedder
    // (regola L): il brain Python non e' piu' coinvolto. Stesso formato di
    // risposta dell'endpoint /embed storico ({"vector":[...]}), nessun cambio
    // nel parsing a valle.
    let client = reqwest::Client::new();
    let resp = client.post(format!("{}/api/embed", state.mcp_core_url))
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
