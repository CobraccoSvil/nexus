// ═══════════════════════════════════════════════════════════════════════════
// knowledge/routes.rs — API REST per la Knowledge Base per-progetto
// ═══════════════════════════════════════════════════════════════════════════

use axum::{
    extract::{Extension, Path as AxumPath, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use nexus_types::{api_error, ensure_project_access, parse_user_id, ApiError, ApiResult};

use crate::{auth::Claims, AppState};

// ── GET /api/projects/:id/knowledge/notes ─────────────────────────────────
#[derive(Deserialize)]
pub struct ListNotesQuery {
    pub status: Option<String>,
    pub intent: Option<String>,
    pub tag: Option<String>,
    pub q: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

pub async fn list_notes(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
    Query(params): Query<ListNotesQuery>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let limit = params.limit.unwrap_or(50).min(200);
    let offset = params.offset.unwrap_or(0);

    // Costruisci query dinamica
    let mut conditions = vec!["project_id = $1".to_string()];
    let mut bind_idx = 2u32;

    if params.status.is_some() {
        conditions.push(format!("status = ${bind_idx}"));
        bind_idx += 1;
    }
    if params.intent.is_some() {
        conditions.push(format!("intent = ${bind_idx}"));
        bind_idx += 1;
    }
    if params.tag.is_some() {
        conditions.push(format!("${bind_idx} = ANY(tags)"));
        bind_idx += 1;
    }
    if params.q.is_some() {
        conditions.push(format!(
            "to_tsvector('simple', coalesce(title,'') || ' ' || coalesce(body_md,'')) @@ plainto_tsquery('simple', ${bind_idx})"
        ));
        bind_idx += 1;
    }

    let where_clause = conditions.join(" AND ");
    let sql = format!(
        r#"
        SELECT id, intent, title, status, tags, file_paths, vault_file_path,
               access_count, created_at, updated_at
        FROM project_knowledge_notes
        WHERE {where_clause}
        ORDER BY created_at DESC
        LIMIT ${bind_idx} OFFSET ${}
        "#,
        bind_idx + 1
    );

    let count_sql = format!(
        "SELECT COUNT(*) as cnt FROM project_knowledge_notes WHERE {where_clause}"
    );

    // Usiamo query raw per la flessibilita' dei bind dinamici
    let mut query = sqlx::query(&sql).bind(project_id);
    let mut count_query = sqlx::query_scalar::<_, i64>(&count_sql).bind(project_id);

    if let Some(ref status) = params.status {
        query = query.bind(status);
        count_query = count_query.bind(status);
    }
    if let Some(ref intent) = params.intent {
        query = query.bind(intent);
        count_query = count_query.bind(intent);
    }
    if let Some(ref tag) = params.tag {
        query = query.bind(tag);
        count_query = count_query.bind(tag);
    }
    if let Some(ref q) = params.q {
        query = query.bind(q);
        count_query = count_query.bind(q);
    }

    query = query.bind(limit).bind(offset);

    let rows = query
        .fetch_all(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let total = count_query
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);

    let notes: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<Uuid, _>("id").to_string(),
                "intent": r.get::<Option<String>, _>("intent"),
                "title": r.get::<String, _>("title"),
                "status": r.get::<String, _>("status"),
                "tags": r.get::<Vec<String>, _>("tags"),
                "filePaths": r.get::<Vec<String>, _>("file_paths"),
                "vaultFilePath": r.get::<Option<String>, _>("vault_file_path"),
                "accessCount": r.get::<i32, _>("access_count"),
                "createdAt": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
                "updatedAt": r.get::<chrono::DateTime<chrono::Utc>, _>("updated_at").to_rfc3339(),
            })
        })
        .collect();

    Ok(Json(json!({
        "notes": notes,
        "total": total,
        "limit": limit,
        "offset": offset,
    })))
}

// ── GET /api/projects/:id/knowledge/notes/:note_id ────────────────────────
pub async fn get_note(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((project_id, note_id)): AxumPath<(String, String)>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let note_id = Uuid::parse_str(&note_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Note id non valido"))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    // Aggiorna access_count e last_accessed_at
    let row = sqlx::query(
        r#"
        UPDATE project_knowledge_notes
        SET access_count = access_count + 1, last_accessed_at = NOW()
        WHERE id = $1 AND project_id = $2
        RETURNING id, project_id, source_run_id, source_message_id, intent, title, body_md,
                  status, tags, file_paths, vault_file_path, access_count,
                  created_at, updated_at, last_accessed_at
        "#,
    )
    .bind(note_id)
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Nota non trovata"))?;

    // Backlinks (note che puntano a questa)
    let backlinks = sqlx::query(
        r#"
        SELECT l.id as link_id, l.from_note_id, l.rel_type, l.created_by, l.confidence,
               n.title as from_title
        FROM project_knowledge_links l
        JOIN project_knowledge_notes n ON n.id = l.from_note_id
        WHERE l.to_note_id = $1
        ORDER BY l.created_at DESC
        "#,
    )
    .bind(note_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    // Outgoing links
    let outgoing = sqlx::query(
        r#"
        SELECT l.id as link_id, l.to_note_id, l.rel_type, l.created_by, l.confidence,
               n.title as to_title
        FROM project_knowledge_links l
        JOIN project_knowledge_notes n ON n.id = l.to_note_id
        WHERE l.from_note_id = $1
        ORDER BY l.created_at DESC
        "#,
    )
    .bind(note_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let backlinks_json: Vec<Value> = backlinks
        .iter()
        .map(|r| {
            json!({
                "linkId": r.get::<Uuid, _>("link_id").to_string(),
                "fromNoteId": r.get::<Uuid, _>("from_note_id").to_string(),
                "fromTitle": r.get::<String, _>("from_title"),
                "relType": r.get::<String, _>("rel_type"),
                "createdBy": r.get::<String, _>("created_by"),
                "confidence": r.get::<f32, _>("confidence"),
            })
        })
        .collect();

    let outgoing_json: Vec<Value> = outgoing
        .iter()
        .map(|r| {
            json!({
                "linkId": r.get::<Uuid, _>("link_id").to_string(),
                "toNoteId": r.get::<Uuid, _>("to_note_id").to_string(),
                "toTitle": r.get::<String, _>("to_title"),
                "relType": r.get::<String, _>("rel_type"),
                "createdBy": r.get::<String, _>("created_by"),
                "confidence": r.get::<f32, _>("confidence"),
            })
        })
        .collect();

    Ok(Json(json!({
        "id": row.get::<Uuid, _>("id").to_string(),
        "projectId": row.get::<Uuid, _>("project_id").to_string(),
        "sourceRunId": row.get::<Option<Uuid>, _>("source_run_id").map(|u| u.to_string()),
        "sourceMessageId": row.get::<Option<Uuid>, _>("source_message_id").map(|u| u.to_string()),
        "intent": row.get::<Option<String>, _>("intent"),
        "title": row.get::<String, _>("title"),
        "bodyMd": row.get::<String, _>("body_md"),
        "status": row.get::<String, _>("status"),
        "tags": row.get::<Vec<String>, _>("tags"),
        "filePaths": row.get::<Vec<String>, _>("file_paths"),
        "vaultFilePath": row.get::<Option<String>, _>("vault_file_path"),
        "accessCount": row.get::<i32, _>("access_count"),
        "createdAt": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
        "updatedAt": row.get::<chrono::DateTime<chrono::Utc>, _>("updated_at").to_rfc3339(),
        "lastAccessedAt": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("last_accessed_at").map(|t| t.to_rfc3339()),
        "backlinks": backlinks_json,
        "outgoing": outgoing_json,
    })))
}

// ── PATCH /api/projects/:id/knowledge/notes/:note_id ──────────────────────
#[derive(Deserialize)]
pub struct PatchNoteBody {
    pub title: Option<String>,
    pub body_md: Option<String>,
    pub tags: Option<Vec<String>>,
    pub status: Option<String>,
}

pub async fn patch_note(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((project_id, note_id)): AxumPath<(String, String)>,
    Json(body): Json<PatchNoteBody>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let note_id = Uuid::parse_str(&note_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Note id non valido"))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    // Validazione status
    if let Some(ref status) = body.status {
        if !["draft", "active", "archived", "deprecated"].contains(&status.as_str()) {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "Status non valido (draft/active/archived/deprecated)",
            ));
        }
    }

    // Costruisci SET clause dinamica
    let mut sets = vec!["updated_at = NOW()".to_string()];
    let mut bind_idx = 3u32;

    if body.title.is_some() {
        sets.push(format!("title = ${bind_idx}"));
        bind_idx += 1;
    }
    if body.body_md.is_some() {
        sets.push(format!("body_md = ${bind_idx}"));
        bind_idx += 1;
    }
    if body.tags.is_some() {
        sets.push(format!("tags = ${bind_idx}"));
        bind_idx += 1;
    }
    if body.status.is_some() {
        sets.push(format!("status = ${bind_idx}"));
        // bind_idx non serve oltre
    }

    let sql = format!(
        "UPDATE project_knowledge_notes SET {} WHERE id = $1 AND project_id = $2 RETURNING id",
        sets.join(", ")
    );

    let mut query = sqlx::query(&sql).bind(note_id).bind(project_id);
    if let Some(ref title) = body.title {
        query = query.bind(title);
    }
    if let Some(ref body_md) = body.body_md {
        query = query.bind(body_md);
    }
    if let Some(ref tags) = body.tags {
        query = query.bind(tags);
    }
    if let Some(ref status) = body.status {
        query = query.bind(status);
    }

    let result = query
        .fetch_optional(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if result.is_none() {
        return Err(api_error(StatusCode::NOT_FOUND, "Nota non trovata"));
    }

    // Clona status prima che venga consumato dal tokio::spawn
    let status_for_emit = body.status.clone();

    // Se body_md cambiato, re-embed su Qdrant
    if let Some(ref new_body) = body.body_md {
        let db_clone = state.db.clone();
        let neural_clone = state.orchestrator.neural.clone();
        let new_body_clone = new_body.clone();
        let status_for_qdrant = status_for_emit.clone();
        tokio::spawn(async move {
            // Recupera qdrant_point_id
            let point_id: Option<String> = sqlx::query_scalar(
                "SELECT qdrant_point_id FROM project_knowledge_notes WHERE id = $1",
            )
            .bind(note_id)
            .fetch_optional(&db_clone)
            .await
            .ok()
            .flatten();

            if let Some(point_id) = point_id {
                let embed_text = if new_body_clone.len() > 2000 {
                    &new_body_clone[..2000]
                } else {
                    &new_body_clone
                };
                if let Ok(vector) = neural_clone.embed_text("", embed_text).await {
                    let payload = json!({
                        "project_id": project_id.to_string(),
                        "note_id": note_id.to_string(),
                        "status": status_for_qdrant.as_deref().unwrap_or("active"),
                    });
                    let _ = crate::vector_memory::upsert_knowledge_point(
                        &db_clone, &point_id, vector, payload,
                    )
                    .await;
                }
            }
        });
    }

    // Emit SSE
    let emit_status = status_for_emit.unwrap_or_else(|| "updated".to_string());
    let _ = nexus_events::dispatcher::emit(
        &state.project_channels,
        project_id,
        nexus_events::ProjectEvent::KnowledgeNoteUpdated {
            note_id,
            status: emit_status,
        },
    );

    Ok(Json(json!({ "ok": true, "noteId": note_id.to_string() })))
}

// ── POST /api/projects/:id/knowledge/similar ──────────────────────────────
#[derive(Deserialize)]
pub struct SimilarBody {
    pub text: String,
}

pub async fn similar_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
    Json(body): Json<SimilarBody>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    if body.text.trim().is_empty() {
        return Ok(Json(json!({ "hits": [] })));
    }

    // Leggi soglia da settings
    let threshold: f64 = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
        .bind("knowledge.similarity_banner_threshold")
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.80);

    let embed_text = if body.text.len() > 2000 {
        &body.text[..2000]
    } else {
        &body.text
    };
    let vector = state
        .orchestrator
        .neural
        .embed_text("", embed_text)
        .await
        .map_err(|e| api_error(StatusCode::SERVICE_UNAVAILABLE, format!("Embedding non disponibile: {e}")))?;

    let raw_hits =
        crate::vector_memory::search_knowledge_points(&state.db, vector, project_id, 5)
            .await
            .map_err(|e| {
                api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("Ricerca fallita: {e}"))
            })?;

    // Filtra per threshold e arricchisci con dati DB
    let mut hits = Vec::new();
    for h in raw_hits {
        if h.score < threshold {
            continue;
        }
        let note_id_str = h
            .payload
            .get("note_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if let Ok(nid) = Uuid::parse_str(note_id_str) {
            // M14.4: risolve anche lo stato di implementazione della nota,
            // seguendo il run collegato sia per source_run_id (agent_summary)
            // sia per source_message_id == agent_runs.run_message_id (note 'chat'
            // della richiesta utente, che nascono senza source_run_id). Privilegia
            // un run 'completed' e il piu' recente. Cosi' il banner puo' dire se la
            // richiesta e' GIA' stata risolta, invece di un generico "note simili".
            let row = sqlx::query(
                r#"
                SELECT n.id, n.title, n.intent, n.status, n.created_at, n.last_accessed_at,
                       r.status AS run_status, r.completed_at AS run_completed_at
                FROM project_knowledge_notes n
                LEFT JOIN LATERAL (
                    SELECT ar.status, ar.completed_at
                    FROM agent_runs ar
                    WHERE ar.id = n.source_run_id
                       OR (ar.run_message_id = n.source_message_id AND ar.project_id = n.project_id)
                    ORDER BY (ar.status = 'completed') DESC, ar.completed_at DESC NULLS LAST
                    LIMIT 1
                ) r ON TRUE
                WHERE n.id = $1 AND n.project_id = $2
                "#,
            )
            .bind(nid)
            .bind(project_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();

            if let Some(r) = row {
                let run_status = r.get::<Option<String>, _>("run_status");
                let implemented = run_status.as_deref() == Some("completed");
                hits.push(json!({
                    "noteId": r.get::<Uuid, _>("id").to_string(),
                    "title": r.get::<String, _>("title"),
                    "intent": r.get::<Option<String>, _>("intent"),
                    "status": r.get::<String, _>("status"),
                    "score": h.score,
                    "createdAt": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
                    "lastAccessedAt": r.get::<Option<chrono::DateTime<chrono::Utc>>, _>("last_accessed_at").map(|t| t.to_rfc3339()),
                    "implemented": implemented,
                    "runStatus": run_status,
                    "runCompletedAt": r.get::<Option<chrono::DateTime<chrono::Utc>>, _>("run_completed_at").map(|t| t.to_rfc3339()),
                }));
            }
        }
    }

    Ok(Json(json!({ "hits": hits })))
}

// ── POST /api/projects/:id/knowledge/links ────────────────────────────────
#[derive(Deserialize)]
pub struct CreateLinkBody {
    pub from_note_id: String,
    pub to_note_id: String,
    pub rel_type: String,
}

pub async fn create_link(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
    Json(body): Json<CreateLinkBody>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let from_id = Uuid::parse_str(&body.from_note_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "from_note_id non valido"))?;
    let to_id = Uuid::parse_str(&body.to_note_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "to_note_id non valido"))?;

    if from_id == to_id {
        return Err(api_error(StatusCode::BAD_REQUEST, "Una nota non puo' linkare se stessa"));
    }

    let valid_types = [
        "followup",
        "correction",
        "refinement",
        "duplicate",
        "blocks",
        "blocked_by",
        "relates",
    ];
    if !valid_types.contains(&body.rel_type.as_str()) {
        return Err(api_error(StatusCode::BAD_REQUEST, "rel_type non valido"));
    }

    let link_id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO project_knowledge_links (id, from_note_id, to_note_id, rel_type, created_by, confidence)
        VALUES ($1, $2, $3, $4, 'user', 1.0)
        ON CONFLICT (from_note_id, to_note_id, rel_type) DO NOTHING
        "#,
    )
    .bind(link_id)
    .bind(from_id)
    .bind(to_id)
    .bind(&body.rel_type)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let _ = nexus_events::dispatcher::emit(
        &state.project_channels,
        project_id,
        nexus_events::ProjectEvent::KnowledgeLinkCreated {
            link_id,
            from: from_id,
            to: to_id,
            rel_type: body.rel_type.clone(),
            created_by: "user".to_string(),
        },
    );

    Ok(Json(json!({ "linkId": link_id.to_string() })))
}

// ── DELETE /api/projects/:id/knowledge/links/:link_id ─────────────────────
pub async fn delete_link(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((project_id, link_id)): AxumPath<(String, String)>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let link_id = Uuid::parse_str(&link_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Link id non valido"))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let deleted = sqlx::query(
        r#"
        DELETE FROM project_knowledge_links
        WHERE id = $1
          AND (created_by = 'user' OR created_by = 'auto')
          AND from_note_id IN (SELECT id FROM project_knowledge_notes WHERE project_id = $2)
        "#,
    )
    .bind(link_id)
    .bind(project_id)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if deleted.rows_affected() == 0 {
        return Err(api_error(StatusCode::NOT_FOUND, "Link non trovato o non eliminabile"));
    }

    Ok(Json(json!({ "ok": true })))
}

// ── DELETE /api/projects/:id/knowledge/notes/:note_id ─────────────────────
//
// Cancella una nota dalla Knowledge Base del progetto. Effetti propagati:
//   - DB: row da `project_knowledge_notes` (CASCADE su `project_knowledge_links`
//     in/out via FK ON DELETE CASCADE).
//   - Qdrant: rimuove l'embedding point associato (se la nota aveva
//     `qdrant_point_id` impostato).
//
// Idempotente: 404 se la nota non esiste/non appartiene al progetto. Lo
// stesso utente deve avere accesso al progetto (ensure_project_access).
pub async fn delete_note(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((project_id, note_id)): AxumPath<(String, String)>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let note_id = Uuid::parse_str(&note_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Note id non valido"))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    // Recupera il qdrant_point_id PRIMA del delete cosi' sappiamo cosa
    // togliere da Qdrant. Se la nota non esiste ritorniamo 404 subito.
    let point_id: Option<String> = sqlx::query_scalar(
        "SELECT qdrant_point_id FROM project_knowledge_notes \
         WHERE id = $1 AND project_id = $2",
    )
    .bind(note_id)
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Nota non trovata"))?;

    // DELETE: i link in/out cascadono via FK ON DELETE CASCADE
    // (vedi project_knowledge_links_{from,to}_note_id_fkey).
    let deleted = sqlx::query(
        "DELETE FROM project_knowledge_notes WHERE id = $1 AND project_id = $2",
    )
    .bind(note_id)
    .bind(project_id)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if deleted.rows_affected() == 0 {
        return Err(api_error(StatusCode::NOT_FOUND, "Nota non trovata"));
    }

    // Best-effort Qdrant cleanup: errori loggati ma non bloccanti.
    // Lo facciamo in background (tokio::spawn) per non rallentare il response,
    // il DB e' gia' coerente. Se Qdrant fallisce restano dei point orfani,
    // rimossi al prossimo restart con vacuum (gestito da janitor separato).
    if let Some(pid) = point_id {
        let db_clone = state.db.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::vector_memory::delete_knowledge_points(
                &db_clone,
                &[pid.clone()],
            )
            .await
            {
                tracing::warn!(
                    "delete_note: cancellazione point Qdrant '{}' fallita: {}",
                    pid, e
                );
            }
        });
    }

    Ok(Json(json!({ "ok": true, "deleted": note_id.to_string() })))
}

// ── GET /api/projects/:id/knowledge/tags ──────────────────────────────────
pub async fn list_tags(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let rows = sqlx::query(
        r#"
        SELECT tag, note_count, last_used_at
        FROM project_knowledge_tags
        WHERE project_id = $1
        ORDER BY note_count DESC, last_used_at DESC
        "#,
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let tags: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "tag": r.get::<String, _>("tag"),
                "noteCount": r.get::<i32, _>("note_count"),
                "lastUsedAt": r.get::<chrono::DateTime<chrono::Utc>, _>("last_used_at").to_rfc3339(),
            })
        })
        .collect();

    Ok(Json(json!({ "tags": tags })))
}

// ── POST /api/internal/knowledge/search ──────────────────────────────────
//
// Endpoint NO-AUTH chiamato dal brain Python per il RAG inline.
// Cerca top-K note rilevanti per il progetto, ritorna title+snippet+score.
//
// Stesso pattern di /api/internal/routing/decide: brain e' isolato dietro
// rete privata, non serve auth per chiamate localhost. Body include
// `project_id` esplicito (il brain lo passa dallo state LangGraph).

#[derive(Deserialize)]
pub struct InternalKbSearchBody {
    pub project_id: String,
    pub query: String,
    pub top_k: Option<usize>,
    pub min_score: Option<f32>,
}

pub async fn internal_kb_search(
    State(state): State<AppState>,
    Json(body): Json<InternalKbSearchBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let project_id = Uuid::parse_str(&body.project_id)
        .map_err(|_| (StatusCode::BAD_REQUEST, "project_id non valido".to_string()))?;
    let query = body.query.trim();
    if query.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "query vuota".to_string()));
    }
    let top_k = body.top_k.unwrap_or(5).clamp(1, 20);
    let min_score = body.min_score.unwrap_or(0.4);

    let embed_text = if query.len() > 2000 { &query[..2000] } else { query };
    let vector = match state.orchestrator.neural.embed_text("", embed_text).await {
        Ok(v) => v,
        Err(e) => {
            return Ok(Json(json!({
                "results": [],
                "warning": format!("embed fallito: {e}"),
            })));
        }
    };

    let hits = match crate::vector_memory::search_knowledge_points(
        &state.db,
        vector,
        project_id,
        top_k * 2,
    )
    .await
    {
        Ok(h) => h,
        Err(e) => {
            return Ok(Json(json!({
                "results": [],
                "warning": format!("Qdrant search fallita: {e}"),
            })));
        }
    };

    let note_hits: Vec<(Uuid, f32)> = hits
        .iter()
        .filter(|h| (h.score as f32) >= min_score)
        .filter_map(|h| {
            h.payload
                .get("note_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<Uuid>().ok())
                .map(|id| (id, h.score as f32))
        })
        .take(top_k)
        .collect();
    if note_hits.is_empty() {
        return Ok(Json(json!({"results": []})));
    }

    let ids: Vec<Uuid> = note_hits.iter().map(|(id, _)| *id).collect();
    let rows = sqlx::query(
        r#"
        SELECT id, title, body_md, tags, intent, status, kind
        FROM project_knowledge_notes
        WHERE id = ANY($1) AND project_id = $2 AND status IN ('active', 'draft')
        "#,
    )
    .bind(&ids)
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("query: {e}")))?;

    let mut by_id: std::collections::HashMap<Uuid, serde_json::Value> =
        std::collections::HashMap::new();
    for r in &rows {
        let id: Uuid = match r.try_get("id") {
            Ok(v) => v,
            Err(_) => continue,
        };
        let body: String = r.try_get("body_md").unwrap_or_default();
        let snippet = body.chars().take(400).collect::<String>();
        by_id.insert(
            id,
            json!({
                "note_id": id.to_string(),
                "title": r.try_get::<String, _>("title").unwrap_or_default(),
                "intent": r.try_get::<Option<String>, _>("intent").ok().flatten(),
                "status": r.try_get::<String, _>("status").unwrap_or_default(),
                "kind": r.try_get::<Option<String>, _>("kind").ok().flatten(),
                "tags": r.try_get::<Vec<String>, _>("tags").unwrap_or_default(),
                "snippet": snippet,
            }),
        );
    }

    let results: Vec<serde_json::Value> = note_hits
        .iter()
        .filter_map(|(id, score)| {
            by_id.get(id).map(|note| {
                let mut n = note.clone();
                n["score"] = json!(*score);
                n
            })
        })
        .collect();

    Ok(Json(json!({"results": results, "count": results.len()})))
}

// ── POST /api/projects/:id/knowledge/rebuild ─────────────────────────────
//
// Ricostruisce la Knowledge Base del progetto reprocessando tutti i messaggi
// user esistenti in `chat_messages`. Utile dopo:
//   - Import progetto da git (le note non sono state create al primo passaggio)
//   - Perdita/svuotamento DB
//   - Cambio della logica di auto-classificazione intent
//
// Pipeline:
//   1. Trova tutti i messaggi user del progetto NON gia' associati a una nota
//      (idempotente: skip messaggi con source_message_id gia' presente)
//   2. Per ciascuno: chiama create_note_from_user_message (genera nota +
//      embedding + upsert Qdrant + scrittura vault .md)
//   3. Al termine: chiama recompute_links_for_project per popolare i link

#[derive(Deserialize)]
pub struct RebuildBody {
    /// Se true, cancella TUTTE le note auto del progetto prima di ricreare.
    /// Default false: idempotente (skip note gia' esistenti).
    pub reset: Option<bool>,
}

pub async fn rebuild_knowledge(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
    body: Option<Json<RebuildBody>>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let body = body.map(|Json(b)| b).unwrap_or(RebuildBody { reset: None });
    let reset = body.reset.unwrap_or(false);

    // Opzionale: cancella note auto esistenti (mantiene quelle curate manualmente
    // identificate da source_message_id IS NULL OR status='active' senza source)
    if reset {
        let deleted = sqlx::query(
            r#"
            DELETE FROM project_knowledge_notes
            WHERE project_id = $1
              AND source_message_id IS NOT NULL
            "#,
        )
        .bind(project_id)
        .execute(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("reset: {e}")))?
        .rows_affected();
        tracing::info!(project_id = %project_id, deleted, "rebuild: reset note auto");
    }

    // Trova messaggi user non ancora con nota associata
    let rows = sqlx::query(
        r#"
        SELECT cm.id, cm.content, cm.metadata
        FROM chat_messages cm
        JOIN chat_sessions cs ON cs.id = cm.session_id
        WHERE cs.project_id = $1
          AND cm.role = 'user'
          AND length(cm.content) >= 10
          AND NOT EXISTS (
            SELECT 1 FROM project_knowledge_notes n
            WHERE n.source_message_id = cm.id
          )
        ORDER BY cm.created_at ASC
        LIMIT 2000
        "#,
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("query messages: {e}")))?;

    // Trova la repo root del progetto per il vault path
    let repo_root: Option<String> = sqlx::query_scalar(
        "SELECT repository_root_path FROM projects WHERE id = $1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    let total = rows.len();
    let mut processed = 0usize;
    let mut skipped_short = 0usize;

    for row in rows {
        let message_id: Uuid = row.try_get("id").unwrap_or_else(|_| Uuid::nil());
        let content: String = row.try_get("content").unwrap_or_default();
        if content.trim().is_empty() {
            skipped_short += 1;
            continue;
        }
        let metadata: serde_json::Value = row
            .try_get("metadata")
            .unwrap_or(serde_json::json!({}));
        let intent = metadata
            .get("intent")
            .and_then(|v| v.as_str())
            .map(String::from);

        // Esegui sincronamente per progress tracking accurato
        if let Err(e) = crate::knowledge::create_note_inner(
            &state.db,
            &state.orchestrator.neural,
            project_id,
            message_id,
            &content,
            intent.as_deref(),
            repo_root.as_deref(),
            &state.project_channels,
        )
        .await
        {
            tracing::debug!(message_id = %message_id, "rebuild: create_note skip: {e}");
            continue;
        }
        processed += 1;
    }

    // Al termine: ricalcola i link
    let (linked_notes, links_created) = crate::knowledge_workers::recompute_links_for_project(
        &state.db,
        &state.orchestrator.neural,
        &state.project_channels,
        project_id,
    )
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("recompute links: {e}")))?;

    Ok(Json(json!({
        "ok": true,
        "reset": reset,
        "messages_total": total,
        "notes_created": processed,
        "skipped_short": skipped_short,
        "linked_notes": linked_notes,
        "links_created": links_created,
    })))
}

// ── POST /api/projects/:id/knowledge/init-or-refresh ────────────────────
//
// **Endpoint unificato** per inizializzare o aggiornare la Knowledge Base
// del progetto in un solo colpo, in modo resiliente (gli errori parziali
// non fanno fallire l'intero processo). Sostituisce il flusso a tre tasti:
//   1. Estrai spec funzionali (chat + file `.md` + sorgenti rilevanti)
//   2. Genera note arricchite tech/functional/test via i 3 generator
//   3. Rebuild idempotente da chat_messages user non ancora notati
//   4. Ricalcola i link automatici sulle note risultanti
//
// Body opzionale `{ "reset": true }` cancella PRIMA tutte le note auto
// (action distruttiva, mantiene solo quelle curate manualmente).
//
// Risposta: stats per ogni fase + warning se qualche step ha fallito.

#[derive(Deserialize)]
pub struct InitRefreshBody {
    /// Se true, cancella tutte le note auto (`source_message_id IS NOT NULL`
    /// o kind in tech/functional/test) prima della rigenerazione.
    pub reset: Option<bool>,
    /// Quanti chat_messages processare per l'extract LLM (default 100, max 500).
    pub chat_limit: Option<i64>,
    /// Massimo file da analizzare per spec funzionali (default 80, max 300).
    pub files_limit: Option<usize>,
}

pub async fn init_or_refresh_knowledge(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
    body: Option<Json<InitRefreshBody>>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let body = body.map(|Json(b)| b).unwrap_or(InitRefreshBody {
        reset: None,
        chat_limit: None,
        files_limit: None,
    });
    let reset = body.reset.unwrap_or(false);
    let chat_limit = body.chat_limit;
    let files_limit = body.files_limit;

    let mut warnings: Vec<String> = Vec::new();
    let mut deleted_count: u64 = 0;

    // Fase 0: reset opzionale (mantiene note manuali con source_message_id NULL
    // e kind non in tech/functional/test)
    if reset {
        match sqlx::query(
            r#"
            DELETE FROM project_knowledge_notes
            WHERE project_id = $1
              AND (
                source_message_id IS NOT NULL
                OR kind IN ('technical','functional','test')
              )
            "#,
        )
        .bind(project_id)
        .execute(&state.db)
        .await
        {
            Ok(r) => {
                deleted_count = r.rows_affected();
                tracing::info!(project_id = %project_id, deleted = deleted_count, "init-or-refresh: reset");
            }
            Err(e) => warnings.push(format!("reset fallito: {e}")),
        }
    }

    // Fase 1: FunctionalSpecAgent (chat + file)
    let mut functional_stats = json!({});
    match crate::knowledge::functional_spec_agent::extract_functional_specs_for_project(
        &state,
        project_id,
        chat_limit,
        true,
        files_limit,
    )
    .await
    {
        Ok(s) => {
            functional_stats = json!({
                "messages_scanned": s.messages_scanned,
                "messages_with_specs": s.messages_with_specs,
                "files_scanned": s.files_scanned,
                "files_with_specs": s.files_with_specs,
                "specs_extracted": s.specs_extracted,
                "specs_applied": s.specs_applied,
                "llm_errors": s.llm_errors,
            });
        }
        Err(e) => warnings.push(format!("functional spec agent: {e}")),
    }

    // Fase 2: 3 generator tech/functional/test
    let mut generators_stats = json!({});
    match crate::knowledge::generators::generate_and_apply_all(&state, project_id).await {
        Ok((total, applied)) => {
            generators_stats = json!({
                "notes_generated": total,
                "notes_applied": applied,
            });
        }
        Err(e) => warnings.push(format!("generators: {e}")),
    }

    // Fase 3: rebuild idempotente da chat_messages
    let mut rebuild_stats = json!({});
    {
        let rows_res = sqlx::query(
            r#"
            SELECT cm.id, cm.content, cm.metadata
            FROM chat_messages cm
            JOIN chat_sessions cs ON cs.id = cm.session_id
            WHERE cs.project_id = $1
              AND cm.role = 'user'
              AND length(cm.content) >= 10
              AND NOT EXISTS (
                SELECT 1 FROM project_knowledge_notes n
                WHERE n.source_message_id = cm.id
              )
            ORDER BY cm.created_at ASC
            LIMIT 2000
            "#,
        )
        .bind(project_id)
        .fetch_all(&state.db)
        .await;

        match rows_res {
            Ok(rows) => {
                let repo_root: Option<String> = sqlx::query_scalar(
                    "SELECT repository_root_path FROM projects WHERE id = $1",
                )
                .bind(project_id)
                .fetch_optional(&state.db)
                .await
                .ok()
                .flatten();

                let total = rows.len();
                let mut processed = 0usize;
                for row in rows {
                    let message_id: Uuid = row.try_get("id").unwrap_or_else(|_| Uuid::nil());
                    let content: String = row.try_get("content").unwrap_or_default();
                    if content.trim().is_empty() {
                        continue;
                    }
                    let metadata: serde_json::Value =
                        row.try_get("metadata").unwrap_or(json!({}));
                    let intent = metadata
                        .get("intent")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    if let Err(e) = crate::knowledge::create_note_inner(
                        &state.db,
                        &state.orchestrator.neural,
                        project_id,
                        message_id,
                        &content,
                        intent.as_deref(),
                        repo_root.as_deref(),
                        &state.project_channels,
                    )
                    .await
                    {
                        tracing::debug!(message_id = %message_id, "init-refresh rebuild skip: {e}");
                        continue;
                    }
                    processed += 1;
                }
                rebuild_stats = json!({
                    "messages_total": total,
                    "notes_created": processed,
                });
            }
            Err(e) => warnings.push(format!("rebuild query: {e}")),
        }
    }

    // Fase 4: ricalcolo link automatici (best-effort)
    let mut links_stats = json!({});
    match crate::knowledge_workers::recompute_links_for_project(
        &state.db,
        &state.orchestrator.neural,
        &state.project_channels,
        project_id,
    )
    .await
    {
        Ok((notes, links)) => {
            links_stats = json!({
                "notes_processed": notes,
                "links_created": links,
            });
        }
        Err(e) => warnings.push(format!("recompute links: {e}")),
    }

    Ok(Json(json!({
        "ok": true,
        "reset": reset,
        "deleted_notes": deleted_count,
        "functional_agent": functional_stats,
        "generators": generators_stats,
        "rebuild_from_chat": rebuild_stats,
        "links": links_stats,
        "warnings": warnings,
    })))
}

// ── POST /api/projects/:id/knowledge/generate-rich ───────────────────────
//
// Esegue i 3 generator (technical/functional/test) e UPSERT le note nella KB
// del progetto. Idempotente: nota esistente con stesso (project_id, kind, title)
// viene UPDATE invece di duplicata.

pub async fn generate_rich_kb(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let (total, applied) =
        crate::knowledge::generators::generate_and_apply_all(&state, project_id)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("generate: {e}")))?;

    // Dopo aver creato le note, ricalcola anche i link automatici
    let (linked_notes, links_created) = crate::knowledge_workers::recompute_links_for_project(
        &state.db,
        &state.orchestrator.neural,
        &state.project_channels,
        project_id,
    )
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("recompute links: {e}")))?;

    Ok(Json(json!({
        "ok": true,
        "notes_generated": total,
        "notes_applied": applied,
        "linked_notes": linked_notes,
        "links_created": links_created,
    })))
}

// ── POST /api/projects/:id/knowledge/recompute-links ─────────────────────
//
// Forza il ricalcolo dei link automatici su TUTTE le note del progetto
// (no filtro temporale). Utile quando le note esistono ma il worker
// periodico non ha ancora generato edge.

pub async fn recompute_links(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let (notes, links) = crate::knowledge_workers::recompute_links_for_project(
        &state.db,
        &state.orchestrator.neural,
        &state.project_channels,
        project_id,
    )
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("recompute: {e}")))?;

    Ok(Json(json!({
        "ok": true,
        "notes_processed": notes,
        "links_created": links,
    })))
}

// ── POST /api/projects/:id/knowledge/notes (creazione manuale nota) ──────
//
// Permette di creare note "funzionali" del progetto (requirement, feature,
// decision, domain, user_story, architecture) non legate alla chat.

#[derive(Deserialize)]
pub struct CreateNoteBody {
    pub title: String,
    pub body_md: String,
    /// intent semantico: feature, requirement, decision, domain, user_story, architecture, ...
    pub intent: Option<String>,
    pub tags: Option<Vec<String>>,
    pub file_paths: Option<Vec<String>>,
}

pub async fn create_note_manual(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
    Json(body): Json<CreateNoteBody>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let title = body.title.trim();
    if title.is_empty() || title.len() > 200 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Titolo obbligatorio (1-200 caratteri)",
        ));
    }
    let body_md = body.body_md.trim();
    if body_md.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "Body obbligatorio"));
    }

    let note_id = Uuid::new_v4();
    let tags = body.tags.unwrap_or_default();
    let file_paths = body.file_paths.unwrap_or_default();
    let intent = body.intent.unwrap_or_else(|| "feature".to_string());

    // Genera embedding e popola Qdrant
    let embed_text = if body_md.len() > 2000 { &body_md[..2000] } else { body_md };
    let qdrant_point_id = match state.orchestrator.neural.embed_text("", embed_text).await {
        Ok(vector) => {
            let point_id = Uuid::new_v4().to_string();
            let payload = json!({
                "project_id": project_id.to_string(),
                "note_id": note_id.to_string(),
                "intent": intent,
                "status": "active",
            });
            match crate::vector_memory::upsert_knowledge_point(&state.db, &point_id, vector, payload).await {
                Ok(_) => Some(point_id),
                Err(e) => {
                    tracing::warn!(error = %e, "knowledge create_note_manual: upsert Qdrant fallito");
                    None
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "knowledge create_note_manual: embed fallito");
            None
        }
    };

    sqlx::query(
        r#"
        INSERT INTO project_knowledge_notes
            (id, project_id, intent, title, body_md, status, qdrant_point_id, tags, file_paths)
        VALUES ($1, $2, $3, $4, $5, 'active', $6, $7, $8)
        "#,
    )
    .bind(note_id)
    .bind(project_id)
    .bind(&intent)
    .bind(title)
    .bind(body_md)
    .bind(qdrant_point_id.as_deref())
    .bind(&tags)
    .bind(&file_paths)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("insert nota: {e}")))?;

    // Aggiorna tag aggregati
    for tag in &tags {
        let _ = sqlx::query(
            r#"
            INSERT INTO project_knowledge_tags (project_id, tag, note_count, last_used_at)
            VALUES ($1, $2, 1, NOW())
            ON CONFLICT (project_id, tag) DO UPDATE SET
                note_count = project_knowledge_tags.note_count + 1,
                last_used_at = NOW()
            "#,
        )
        .bind(project_id)
        .bind(tag)
        .execute(&state.db)
        .await;
    }

    let _ = nexus_events::dispatcher::emit(
        &state.project_channels,
        project_id,
        nexus_events::ProjectEvent::KnowledgeNoteCreated {
            note_id,
            title: title.to_string(),
            intent: Some(intent.clone()),
        },
    );

    Ok(Json(json!({
        "ok": true,
        "note_id": note_id,
        "intent": intent,
    })))
}

// ── GET/PUT /api/projects/:id/knowledge/obsidian-vault ───────────────────
//
// GET ritorna il nome del vault Obsidian configurato per il progetto.
// PUT aggiorna il nome (stringa vuota = reset).

pub async fn get_obsidian_vault(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let row = sqlx::query("SELECT obsidian_vault_name FROM projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("query: {e}")))?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Progetto non trovato"))?;

    let name: String = row.try_get("obsidian_vault_name").unwrap_or_default();
    Ok(Json(json!({ "obsidian_vault_name": name })))
}

#[derive(Deserialize)]
pub struct ObsidianVaultBody {
    pub obsidian_vault_name: String,
}

pub async fn put_obsidian_vault(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
    Json(body): Json<ObsidianVaultBody>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    // Validation: max 100 char, no spazi all'inizio/fine, niente caratteri di filesystem
    let name = body.obsidian_vault_name.trim().to_string();
    if name.len() > 100 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Nome vault troppo lungo (max 100 char)",
        ));
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Nome vault contiene caratteri invalidi",
        ));
    }

    sqlx::query("UPDATE projects SET obsidian_vault_name = $1 WHERE id = $2")
        .bind(&name)
        .bind(project_id)
        .execute(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("update: {e}")))?;

    Ok(Json(json!({ "ok": true, "obsidian_vault_name": name })))
}

// ── POST /api/projects/:id/knowledge/extract-functional ──────────────────
//
// Esegue il FunctionalSpecAgent: scansiona i chat_messages user del progetto
// e usa LLM (purpose `functional_spec_extractor`) per estrarre specifiche
// funzionali concrete (feature/requirement/user_story/decision/domain/...).
// Le specifiche estratte vengono materializzate come note kind='functional'
// nella KB del progetto.

#[derive(Deserialize)]
pub struct ExtractFunctionalBody {
    /// Quanti chat_messages user processare (default 50, max 500).
    pub limit: Option<i64>,
    /// Se true, scansiona anche i file `.md` + sorgenti chiave del repository
    /// del progetto (default true — utile alla prima inizializzazione).
    pub include_files: Option<bool>,
    /// Massimo file da analizzare (default 80, max 300).
    pub files_limit: Option<usize>,
}

pub async fn extract_functional_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
    body: Option<Json<ExtractFunctionalBody>>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let (limit, include_files, files_limit) = body
        .map(|Json(b)| (b.limit, b.include_files.unwrap_or(true), b.files_limit))
        .unwrap_or((None, true, None));

    let stats = crate::knowledge::functional_spec_agent::extract_functional_specs_for_project(
        &state,
        project_id,
        limit,
        include_files,
        files_limit,
    )
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("extract: {e}")))?;

    // Dopo aver creato note funzionali, ricalcola i link
    let (linked_notes, links_created) = crate::knowledge_workers::recompute_links_for_project(
        &state.db,
        &state.orchestrator.neural,
        &state.project_channels,
        project_id,
    )
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("recompute links: {e}")))?;

    Ok(Json(json!({
        "ok": true,
        "messages_scanned": stats.messages_scanned,
        "messages_skipped_short": stats.messages_skipped_short,
        "messages_with_specs": stats.messages_with_specs,
        "files_scanned": stats.files_scanned,
        "files_skipped_short": stats.files_skipped_short,
        "files_with_specs": stats.files_with_specs,
        "specs_extracted": stats.specs_extracted,
        "specs_applied": stats.specs_applied,
        "llm_errors": stats.llm_errors,
        "linked_notes": linked_notes,
        "links_created": links_created,
    })))
}

// ── GET /api/projects/:id/knowledge/graph ────────────────────────────────
//
// Ritorna nodi + edge per la visualizzazione graph del Knowledge vault.
// Query params opzionali:
//   - center: id nota su cui centrare (per future espansioni focused-view)
//   - depth: profondita' max hop dal center (default tutto)
//   - kind:  filtro per status (active/draft/archived/deprecated)
//   - min_confidence: nasconde edge con confidence < soglia (default 0.0)
//
// Implementazione MVP: ritorna TUTTI i nodi + TUTTI gli edge del progetto
// (graph completo). Il filtering avanzato per center/depth e' lasciato al
// client (Cytoscape supporta bene il client-side filtering).

#[derive(Deserialize)]
pub struct GraphQuery {
    pub status: Option<String>,
    pub min_confidence: Option<f32>,
}

pub async fn graph_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
    Query(params): Query<GraphQuery>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let status_filter = params.status.unwrap_or_default();
    let min_conf = params.min_confidence.unwrap_or(0.0);

    let nodes = sqlx::query(
        r#"
        SELECT id, title, intent, status, tags, access_count, updated_at
        FROM project_knowledge_notes
        WHERE project_id = $1
          AND ($2 = '' OR status = $2)
        ORDER BY updated_at DESC
        LIMIT 1500
        "#,
    )
    .bind(project_id)
    .bind(&status_filter)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("nodes query: {e}")))?;

    let nodes_json: Vec<Value> = nodes
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<Uuid, _>("id").ok(),
                "title": r.try_get::<String, _>("title").unwrap_or_default(),
                "intent": r.try_get::<Option<String>, _>("intent").ok().flatten(),
                "status": r.try_get::<String, _>("status").unwrap_or_default(),
                "tags": r.try_get::<Vec<String>, _>("tags").unwrap_or_default(),
                "access_count": r.try_get::<i32, _>("access_count").unwrap_or(0),
                "updated_at": r.try_get::<chrono::DateTime<chrono::Utc>, _>("updated_at").ok(),
            })
        })
        .collect();

    let edges = sqlx::query(
        r#"
        SELECT l.id, l.from_note_id, l.to_note_id, l.rel_type, l.created_by, l.confidence
        FROM project_knowledge_links l
        JOIN project_knowledge_notes n1 ON n1.id = l.from_note_id
        JOIN project_knowledge_notes n2 ON n2.id = l.to_note_id
        WHERE n1.project_id = $1
          AND n2.project_id = $1
          AND l.confidence >= $2
        LIMIT 5000
        "#,
    )
    .bind(project_id)
    .bind(min_conf)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("edges query: {e}")))?;

    let edges_json: Vec<Value> = edges
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<Uuid, _>("id").ok(),
                "from": r.try_get::<Uuid, _>("from_note_id").ok(),
                "to": r.try_get::<Uuid, _>("to_note_id").ok(),
                "rel_type": r.try_get::<String, _>("rel_type").unwrap_or_default(),
                "created_by": r.try_get::<String, _>("created_by").unwrap_or_default(),
                "confidence": r.try_get::<f32, _>("confidence").unwrap_or(0.0),
            })
        })
        .collect();

    Ok(Json(json!({
        "nodes": nodes_json,
        "edges": edges_json,
        "stats": {
            "nodes_count": nodes_json.len(),
            "edges_count": edges_json.len(),
        }
    })))
}
