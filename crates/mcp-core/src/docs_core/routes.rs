// ═══════════════════════════════════════════════════════════════════════════
// docs_core/routes.rs — Handler HTTP wiki condivisi.
//
// Espone:
//   - PATCH /api/meta-docs/:id                       (nuovo: edit manuale meta)
//   - GET   /api/projects/:pid/knowledge/notes/:id/revisions
//   - GET   /api/projects/:pid/knowledge/notes/:id/revisions/:version
//   - GET   /api/projects/:pid/knowledge/notes/:id/diff?from=&to=
//   - POST  /api/projects/:pid/knowledge/notes/:id/restore   { version }
//
// Gli handler riusano `storage::update_doc` e `revisions::*` (scope-agnostici).
// L'auth meta usa `require_auth`; l'auth progetti aggiunge ensure_project_access.
// ═══════════════════════════════════════════════════════════════════════════

use crate::auth::Claims;
use crate::docs_core::revisions::{get_revision, list_revisions, DocScope};
use crate::docs_core::storage::{update_doc, DocPatch};
use crate::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use nexus_types::{api_error, ensure_project_access, parse_user_id, ApiResult};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

// ─────────────────────────────── META ────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct MetaPatchBody {
    pub title: Option<String>,
    pub body_md: Option<String>,
    pub tags: Option<Vec<String>>,
}

/// `PATCH /api/meta-docs/:id` — edit manuale di un meta-doc.
pub async fn patch_meta_doc(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<MetaPatchBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let patch = DocPatch {
        title: body.title,
        body_md: body.body_md,
        tags: body.tags,
        status: None,
        revision_source: None,
        edit_summary: None,
    };
    let out = update_doc(&state, DocScope::Meta, id, &claims.sub, patch)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            let code = if msg.contains("non trovato") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (code, msg)
        })?;
    Ok(Json(json!({
        "ok": true,
        "id": id.to_string(),
        "version": out.version_no,
        "body_changed": out.body_changed,
    })))
}

// ─────────────────────────────── PROGETTO ────────────────────────────────

async fn ensure_note_in_project(
    state: &AppState,
    project_id: Uuid,
    note_id: Uuid,
) -> Result<(), nexus_types::ApiError> {
    let row = sqlx::query("SELECT 1 FROM project_knowledge_notes WHERE id = $1 AND project_id = $2")
        .bind(note_id)
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if row.is_none() {
        return Err(api_error(StatusCode::NOT_FOUND, "Nota non trovata"));
    }
    Ok(())
}

async fn parse_and_auth(
    state: &AppState,
    claims: &Claims,
    project_id_str: &str,
    note_id_str: &str,
) -> Result<(Uuid, Uuid), nexus_types::ApiError> {
    let user_id = parse_user_id(claims)?;
    let project_id = Uuid::parse_str(project_id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let note_id = Uuid::parse_str(note_id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Note id non valido"))?;
    ensure_project_access(&state.db, user_id, project_id).await?;
    ensure_note_in_project(state, project_id, note_id).await?;
    Ok((project_id, note_id))
}

/// `GET /api/projects/:pid/knowledge/notes/:id/revisions`
pub async fn proj_list_revisions(
    State(state): State<AppState>,
    Path((pid, nid)): Path<(String, String)>,
    Extension(claims): Extension<Claims>,
) -> ApiResult {
    let (project_id, note_id) = parse_and_auth(&state, &claims, &pid, &nid).await?;
    let items = list_revisions(&state.db, DocScope::Project(project_id), note_id)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let total = items.len();
    Ok(Json(json!({ "items": items, "total": total })))
}

/// `GET /api/projects/:pid/knowledge/notes/:id/revisions/:version`
pub async fn proj_get_revision(
    State(state): State<AppState>,
    Path((pid, nid, version)): Path<(String, String, i32)>,
    Extension(claims): Extension<Claims>,
) -> ApiResult {
    let (project_id, note_id) = parse_and_auth(&state, &claims, &pid, &nid).await?;
    let rev = get_revision(&state.db, DocScope::Project(project_id), note_id, version)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Revisione non trovata"))?;
    Ok(Json(serde_json::to_value(rev).unwrap_or_else(|_| json!({}))))
}

#[derive(Debug, Deserialize)]
pub struct ProjDiffQuery {
    pub from: i32,
    pub to: i32,
}

/// `GET /api/projects/:pid/knowledge/notes/:id/diff?from=&to=`
pub async fn proj_diff(
    State(state): State<AppState>,
    Path((pid, nid)): Path<(String, String)>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<ProjDiffQuery>,
) -> ApiResult {
    let (project_id, note_id) = parse_and_auth(&state, &claims, &pid, &nid).await?;
    let from = get_revision(&state.db, DocScope::Project(project_id), note_id, q.from)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Revisione 'from' non trovata"))?;
    let to = get_revision(&state.db, DocScope::Project(project_id), note_id, q.to)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Revisione 'to' non trovata"))?;
    Ok(Json(json!({ "from": from, "to": to })))
}

#[derive(Debug, Deserialize)]
pub struct ProjRestoreBody {
    pub version: i32,
}

/// `POST /api/projects/:pid/knowledge/notes/:id/restore` — ripristina una
/// revisione precedente come nuova revisione (source=revert).
pub async fn proj_restore(
    State(state): State<AppState>,
    Path((pid, nid)): Path<(String, String)>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<ProjRestoreBody>,
) -> ApiResult {
    let (project_id, note_id) = parse_and_auth(&state, &claims, &pid, &nid).await?;
    let target = get_revision(&state.db, DocScope::Project(project_id), note_id, body.version)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Revisione non trovata"))?;

    let patch = DocPatch {
        title: Some(target.title.clone()),
        body_md: Some(target.body_md.clone()),
        tags: Some(target.tags.clone()),
        status: None,
        revision_source: Some("revert"),
        edit_summary: Some(format!("restore della revisione v{}", body.version)),
    };
    let out = update_doc(&state, DocScope::Project(project_id), note_id, &claims.sub, patch)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({
        "ok": true,
        "restored_from": body.version,
        "version": out.version_no,
    })))
}
