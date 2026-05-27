use axum::{
    extract::{Extension, Path as AxumPath, Query, State},
    http::StatusCode,
    Json,
};
use serde_json::{json, Value};
use tokio::fs;
use uuid::Uuid;

use crate::{auth::Claims, AppState};
use crate::projects::{
    api_error, load_project_context, parse_user_id, resolve_relative_path,
    resolve_workspace_target, to_relative, upsert_open_session, list_directory_nodes,
    CreateEntryRequest, DeleteEntryRequest, FileQuery, RenameEntryRequest, SaveFileRequest,
    SearchQuery, TreeQuery, EXCLUDED_NAMES,
};

type ApiError = (StatusCode, Json<Value>);
type ApiResult = Result<Json<Value>, ApiError>;

pub async fn get_project_tree(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<TreeQuery>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let target = match query.path.as_deref() {
        Some(path) if !path.trim().is_empty() => resolve_relative_path(&context.root_path, path)?,
        _ => context.root_path.clone(),
    };
    // list_directory_nodes fa I/O sincrono intensivo: spawn_blocking
    // per non bloccare il runtime tokio.
    let root_for_tree = context.root_path.clone();
    let nodes = tokio::task::spawn_blocking(move || {
        list_directory_nodes(&root_for_tree, &target)
    })
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("spawn_blocking tree: {e}")))?
    ?;

    Ok(Json(json!({
        "path": query.path.unwrap_or_default(),
        "nodes": nodes,
    })))
}

pub async fn get_project_file(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<FileQuery>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let file_path = resolve_relative_path(&context.root_path, &query.path)?;
    if !file_path.is_file() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il percorso richiesto non e' un file",
        ));
    }

    let content = fs::read_to_string(&file_path).await.map_err(|_| {
        api_error(
            StatusCode::BAD_REQUEST,
            "Impossibile leggere il file come testo UTF-8",
        )
    })?;

    upsert_open_session(
        &state.db,
        user_id,
        &context,
        &[to_relative(&context.root_path, &file_path)],
        context.details.root_path.as_deref(),
    )
    .await?;

    Ok(Json(json!({
        "path": to_relative(&context.root_path, &file_path),
        "content": content,
    })))
}

pub async fn save_project_file(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<SaveFileRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    if !context.access.can_write {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Non hai permessi di scrittura su questo progetto",
        ));
    }

    let file_path = resolve_relative_path(&context.root_path, &body.path)?;
    fs::write(&file_path, &body.content)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    upsert_open_session(
        &state.db,
        user_id,
        &context,
        &[to_relative(&context.root_path, &file_path)],
        context.details.root_path.as_deref(),
    )
    .await?;

    Ok(Json(json!({
        "saved": true,
        "path": to_relative(&context.root_path, &file_path),
    })))
}

pub async fn create_project_entry(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<CreateEntryRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    if !context.access.can_write {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Non hai permessi di scrittura su questo progetto",
        ));
    }

    let (relative_path, target_path) = resolve_workspace_target(&context.root_path, &body.path)?;
    if target_path.exists() {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Esiste gia' un file o directory con questo percorso",
        ));
    }

    let parent = target_path.parent().ok_or_else(|| {
        api_error(
            StatusCode::BAD_REQUEST,
            "Impossibile determinare la cartella padre",
        )
    })?;
    if !parent.exists() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "La cartella padre non esiste",
        ));
    }

    match body.kind.trim() {
        "directory" => {
            fs::create_dir(&target_path)
                .await
                .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
        "file" => {
            fs::write(&target_path, body.content.as_deref().unwrap_or_default())
                .await
                .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
        _ => {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "Il tipo deve essere 'file' o 'directory'",
            ));
        }
    }

    upsert_open_session(
        &state.db,
        user_id,
        &context,
        &[relative_path.clone()],
        context.details.root_path.as_deref(),
    )
    .await?;

    Ok(Json(json!({
        "ok": true,
        "path": relative_path,
        "kind": body.kind.trim(),
    })))
}

pub async fn rename_project_entry(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<RenameEntryRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    if !context.access.can_write {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Non hai permessi di scrittura su questo progetto",
        ));
    }

    let (_, old_path) = resolve_workspace_target(&context.root_path, &body.old_path)?;
    if !old_path.exists() {
        return Err(api_error(StatusCode::NOT_FOUND, "Il percorso da rinominare non esiste"));
    }
    let (new_relative_path, new_path) = resolve_workspace_target(&context.root_path, &body.new_path)?;
    if new_path.exists() {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Il nuovo percorso esiste gia'",
        ));
    }
    let parent = new_path.parent().ok_or_else(|| {
        api_error(
            StatusCode::BAD_REQUEST,
            "Impossibile determinare la cartella padre",
        )
    })?;
    if !parent.exists() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "La cartella padre del nuovo percorso non esiste",
        ));
    }

    fs::rename(&old_path, &new_path)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    upsert_open_session(
        &state.db,
        user_id,
        &context,
        &[new_relative_path.clone()],
        context.details.root_path.as_deref(),
    )
    .await?;

    Ok(Json(json!({
        "ok": true,
        "oldPath": body.old_path.trim().replace('\\', "/"),
        "newPath": new_relative_path,
    })))
}

pub async fn delete_project_entry(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<DeleteEntryRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    if !context.access.can_write {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Non hai permessi di scrittura su questo progetto",
        ));
    }

    let (relative_path, target_path) = resolve_workspace_target(&context.root_path, &body.path)?;
    if !target_path.exists() {
        return Err(api_error(StatusCode::NOT_FOUND, "Il percorso da eliminare non esiste"));
    }

    let metadata = fs::metadata(&target_path)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if metadata.is_dir() {
        fs::remove_dir_all(&target_path)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    } else {
        fs::remove_file(&target_path)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    Ok(Json(json!({
        "ok": true,
        "path": relative_path,
    })))
}

pub async fn search_project(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<SearchQuery>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let term = query.q.trim();
    if term.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il testo di ricerca e' obbligatorio",
        ));
    }

    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let root_path = context.root_path.clone();
    let term_owned = term.to_string();
    let matches = tokio::task::spawn_blocking(move || {
        let mut stack = vec![root_path.clone()];
        let mut matches = Vec::new();
        let term_lower = term_owned.to_lowercase();
        while let Some(path) = stack.pop() {
            if matches.len() >= limit {
                break;
            }
            let entries = match std::fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for entry in entries.filter_map(|entry| entry.ok()) {
                if matches.len() >= limit {
                    break;
                }
                let child_path = entry.path();
                let metadata = match entry.metadata() {
                    Ok(metadata) => metadata,
                    Err(_) => continue,
                };
                let name = entry.file_name().to_string_lossy().to_string();
                if EXCLUDED_NAMES.contains(&name.as_str()) {
                    continue;
                }
                if metadata.is_dir() {
                    stack.push(child_path);
                    continue;
                }
                if !metadata.is_file() {
                    continue;
                }
                // ── 1. Match per NOME FILE (path basename + path relativo) ──
                // Permette di cercare "function_report.txt" e trovare il file
                // anche se il contenuto non contiene quella stringa. Match
                // case-insensitive sia sul basename che sul path relativo.
                let rel_path = to_relative(&root_path, &child_path);
                let name_lower = name.to_lowercase();
                let rel_lower = rel_path.to_lowercase();
                let name_match = name_lower.contains(&term_lower) || rel_lower.contains(&term_lower);
                if name_match {
                    matches.push(json!({
                        "path": rel_path.clone(),
                        "line": 0,
                        "column": 0,
                        "preview": format!("[file] {}", name),
                        "kind": "filename",
                    }));
                    if matches.len() >= limit {
                        break;
                    }
                }
                // ── 2. Match per CONTENUTO file (linee testuali) ──
                let content = match std::fs::read_to_string(&child_path) {
                    Ok(content) => content,
                    Err(_) => continue,
                };
                for (index, line) in content.lines().enumerate() {
                    if matches.len() >= limit {
                        break;
                    }
                    if let Some(column) = line.find(&*term_owned) {
                        matches.push(json!({
                            "path": rel_path.clone(),
                            "line": index + 1,
                            "column": column + 1,
                            "preview": line.trim(),
                            "kind": "content",
                        }));
                    }
                }
            }
        }
        matches
    })
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("Ricerca fallita: {e}")))?;

    Ok(Json(json!({
        "query": term,
        "results": matches,
    })))
}
