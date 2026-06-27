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
use tokio::fs;

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
///
/// DEPRECATO (410 Gone). Questo handler duplicava la generazione documenti di
/// mcp-core ma renderizzava il .docx chiamando il brain Python via REST
/// (`POST {brain}/generate-document`) — un percorso AI-adiacente al brain, gia'
/// rotto e mai instradato a runtime (vedi `apps/web-ide/next.config.ts` e
/// `deploy/nginx-microservices.conf`: le route `/api/documents/*` cadono nel
/// fallback verso mcp-core, porta 4000).
///
/// La generazione documenti vive ESCLUSIVAMENTE in mcp-core, che ora renderizza
/// il .docx in-process in Rust (`crate::docx_render`, punto unico regola L) senza
/// alcun round-trip al brain (verso zero-Python). Per evitare di mantenere QUI un
/// secondo renderer che riapre la dipendenza dal brain (regola H: niente toppe,
/// niente codice morto che chiama Python), l'endpoint risponde 410 indirizzando
/// al percorso canonico. Il frontend non lo chiama (usa l'endpoint mcp-core).
pub async fn generate_document(
    State(_state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(_req): Json<Value>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    Err(api_error(
        StatusCode::GONE,
        "Endpoint deprecato: la generazione documenti e' servita da mcp-core \
         (POST /api/projects/:id/documents/generate). doc-service non genera piu' \
         documenti.",
    ))
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
