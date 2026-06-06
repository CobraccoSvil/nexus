use axum::{
    body::Body,
    extract::{Extension, Path as AxumPath, State},
    http::{header, StatusCode},
    Json,
};
use serde_json::{json, Value};
use sqlx::Row;
use tokio::fs;
use uuid::Uuid;

use crate::{
    auth::Claims,
    chat_learning::{api_error, parse_user_id, ApiError, ApiResult},
    projects::load_project_context,
    AppState,
};

/// GET /api/projects/:id/documents
///
/// Restituisce la lista documenti dal DB project_documents arricchita
/// con auto-discovery dei .md presenti in `docs/` ma non ancora catalogati.
/// I file orfani vengono inseriti nel DB con status='draft' e doc_type
/// inferito dal nome (technical_analysis, functional_analysis, ecc.) per
/// essere visibili nel pannello DOCUMENTI anche se generati da write_file.
pub async fn list_documents(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    // Auto-discovery: scansiona la cartella docs/ e auto-registra i file
    // orfani (presenti sul filesystem ma assenti dal DB).
    // FIX 2: include i .docx oltre ai .md. Il flusso canonico
    // (nexus_doc_generate) salva .docx; prima la discovery li ignorava, quindi
    // un .docx orfano (es. INSERT mai avvenuto) non veniva mai recuperato.
    if let Ok(ctx) = load_project_context(&state.db, project_id, user_id).await {
        let docs_dir = ctx.root_path.join("docs");
        if let Ok(mut entries) = fs::read_dir(&docs_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                    continue;
                };
                if !matches!(ext.to_ascii_lowercase().as_str(), "md" | "docx") {
                    continue;
                }
                let Some(file_name) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let path_str = path.to_string_lossy().to_string();

                // Inferisci doc_type dal nome file.
                let doc_type = infer_doc_type(file_name);
                let title = humanize_filename(file_name);

                // INSERT atomico con guardia NOT EXISTS: previene duplicati anche
                // sotto chiamate concorrenti (React StrictMode invoca l'effect due
                // volte). Senza UNIQUE constraint sul DB, due SELECT + INSERT
                // separati creerebbero record duplicati.
                let _ = sqlx::query(
                    "INSERT INTO project_documents
                     (project_id, doc_type, title, version, file_path, status, metadata)
                     SELECT $1, $2, $3, '1.0.0', $4, 'draft', $5
                     WHERE NOT EXISTS (
                         SELECT 1 FROM project_documents
                         WHERE project_id = $1 AND file_path = $4
                     )",
                )
                .bind(project_id)
                .bind(&doc_type)
                .bind(&title)
                .bind(&path_str)
                .bind(json!({ "source": "auto_discovery", "discovered_at": chrono::Utc::now().to_rfc3339() }))
                .execute(&state.db)
                .await;
            }
        }
    }

    let rows = sqlx::query(
        "SELECT id, project_id, doc_type, title, version, file_path, status, metadata, created_at, updated_at
         FROM project_documents WHERE project_id = $1 ORDER BY doc_type, updated_at DESC",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("DB error: {e}")))?;

    let docs: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
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
            })
        })
        .collect();

    Ok(Json(json!({ "documents": docs })))
}

/// Inferisce il doc_type dal nome del file. Default: technical_analysis
/// (compatibile con il check constraint del DB).
fn infer_doc_type(file_stem: &str) -> String {
    let lower = file_stem.to_lowercase();
    if lower.contains("funzionale") || lower.contains("functional") {
        "functional_analysis".to_string()
    } else if lower.contains("er") && (lower.contains("diagram") || lower.contains("model")) {
        "er_diagram".to_string()
    } else if lower.contains("project_management")
        || lower.contains("gestione")
        || lower.contains("piano")
    {
        "project_management".to_string()
    } else if lower.contains("release") {
        "release_notes".to_string()
    } else {
        // Default: tutto il resto e' technical_analysis (es. README, governance,
        // threat-model, ecc.). Il check constraint del DB vincola a 5 valori.
        "technical_analysis".to_string()
    }
}

/// Trasforma `analisi-tecnica-redemptor` -> `Analisi Tecnica Redemptor`.
fn humanize_filename(file_stem: &str) -> String {
    file_stem
        .replace(['_', '-'], " ")
        .split_whitespace()
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().chain(chars).collect(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// GET /api/projects/:id/documents/:doc_id
pub async fn get_document(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((id, doc_id)): AxumPath<(String, String)>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let _project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let document_id = Uuid::parse_str(&doc_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Document id non valido"))?;

    let row = sqlx::query(
        "SELECT id, project_id, doc_type, title, version, file_path, structure_json, status, metadata, created_at, updated_at
         FROM project_documents WHERE id = $1",
    )
    .bind(document_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("DB error: {e}")))?
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

    let row = sqlx::query(
        "SELECT file_path, title FROM project_documents WHERE id = $1 AND project_id = $2",
    )
    .bind(document_id)
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("DB error: {e}")))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Documento non trovato"))?;

    let file_path: String = row.get("file_path");
    let _title: String = row.get("title");

    // Resolve absolute path from project root
    let context = load_project_context(&state.db, project_id, parse_user_id(&claims)?).await?;
    let abs_path = context.root_path.join(&file_path);

    if !abs_path.exists() {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "File documento non trovato sul filesystem",
        ));
    }

    let bytes = fs::read(&abs_path).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Errore lettura file: {e}"),
        )
    })?;

    let filename = abs_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document.docx");

    let response = axum::response::Response::builder()
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
        .map_err(|e| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Response error: {e}"),
            )
        })?;

    Ok(response)
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
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("DB error: {e}")))?;

    let versions: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<Uuid, _>("id").to_string(),
                "version": r.get::<String, _>("version"),
                "file_path": r.get::<String, _>("file_path"),
                "change_summary": r.get::<Option<String>, _>("change_summary"),
                "changed_sections": r.get::<Option<Vec<String>>, _>("changed_sections"),
                "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
            })
        })
        .collect();

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

    // Get file path and qdrant points before deleting
    let row = sqlx::query(
        "SELECT file_path, qdrant_point_ids FROM project_documents WHERE id = $1 AND project_id = $2",
    )
    .bind(document_id)
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("DB error: {e}")))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Documento non trovato"))?;

    let file_path: String = row.get("file_path");
    let qdrant_point_ids: Vec<String> = row.get("qdrant_point_ids");

    // Delete from DB (cascade deletes versions too)
    sqlx::query("DELETE FROM project_documents WHERE id = $1")
        .bind(document_id)
        .execute(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("DB error: {e}")))?;

    // Delete file from filesystem
    let context = load_project_context(&state.db, project_id, parse_user_id(&claims)?).await?;
    let abs_path = context.root_path.join(&file_path);
    let _ = fs::remove_file(&abs_path).await;

    // Delete Qdrant points
    if !qdrant_point_ids.is_empty() {
        let _ = crate::vector_memory::delete_doc_points_by_ids(&state.db, &qdrant_point_ids).await;
    }

    Ok(Json(json!({ "deleted": true })))
}

/// POST /api/projects/:id/documents/generate
///
/// FIX 3/4: generazione documento SENZA passare per l'agente conversazionale.
/// Prima il pulsante "Genera" del pannello DOCUMENTI inviava un messaggio in
/// chat (`onSendToChat`) che instradava la richiesta sull'agente generico: dopo
/// la chiamata al tool l'agente non era vincolato a fermarsi e proseguiva con
/// una "revisione" del progetto non richiesta; inoltre il pannello si
/// aggiornava solo a fine turno (evento window `nexus:documents:refresh`),
/// quindi con timing impredicibile.
///
/// Questo endpoint chiama direttamente `nexus_builtin::handle_doc_generate`
/// (stesso punto unico usato dal tool) e ritorna l'esito sincrono: il frontend
/// puo' fare il refresh subito, deterministico.
pub async fn generate_document(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    let doc_type = body
        .get("doc_type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Parametro 'doc_type' obbligatorio"))?;
    let title = body
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    // Costruisce gli stessi argomenti del tool nexus_doc_generate. content_json
    // omesso: il backend auto-genera (con KB injection, FIX 1).
    let args = json!({
        "project_id": project_id.to_string(),
        "doc_type": doc_type,
        "title": title,
    });

    // Generazione ASINCRONA: con un modello heavy/thinking la completion puo'
    // durare minuti. Se la legassimo alla connessione HTTP, il proxy la
    // chiuderebbe in timeout (-> 500) e axum cancellerebbe l'handler a meta',
    // lasciando nessun documento. Avviamo in background e ritorniamo subito 202:
    // il completamento arriva al pannello via evento SSE DocumentGenerated (su
    // successo) o Notification (su errore), gia' ascoltati dal frontend.
    let db = state.db.clone();
    let doc_type_for_log = doc_type.clone();
    tokio::spawn(async move {
        let result =
            crate::nexus_builtin::handle_doc_generate(&db, project_id, user_id, &args).await;
        if let Some(msg) = result.strip_prefix("[Errore]") {
            let msg = msg.trim().to_string();
            tracing::warn!(doc_type = %doc_type_for_log, "generate_document (async): {msg}");
            // Notifica il fallimento al pannello (toast), altrimenti l'utente
            // resterebbe in attesa di un documento che non arrivera' mai.
            let _ = nexus_events::dispatcher::emit_global(
                project_id,
                nexus_events::event::ProjectEvent::Notification {
                    severity: "error".to_string(),
                    message: format!("Generazione documento fallita: {msg}"),
                    panel: Some("documents".to_string()),
                    ttl_ms: Some(10000),
                    run_id: None,
                },
            );
        }
        // Su successo handle_doc_generate ha gia' emesso DocumentGenerated.
    });

    Ok(Json(json!({
        "status": "accepted",
        "message": "Generazione avviata: il documento comparira' nel pannello al termine."
    })))
}
