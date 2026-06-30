use axum::{
    extract::{Extension, Path as AxumPath, Query, State},
    http::StatusCode,
    Json,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::projects::{
    api_error, execute_git_paths_operation, execute_git_remote_operation, load_project_context,
    parse_branch_line, parse_user_id, record_git_operation, refresh_git_snapshot, run_git_command,
    GitCheckoutRequest, GitCommitRequest, GitCreateBranchRequest, GitDiffQuery, GitLogEntry,
    GitPathsRequest, GitRemoteRequest, GitUiPreferencesUpdateRequest,
};
use crate::{auth::Claims, AppState};

type ApiError = (StatusCode, Json<Value>);
type ApiResult = Result<Json<Value>, ApiError>;

pub async fn git_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let git_state = refresh_git_snapshot(&state.db, &context).await?;

    Ok(Json(json!({
        "projectId": context.details.id,
        "canManageGit": context.access.can_manage_git,
        "git": git_state,
    })))
}

pub async fn git_branches(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;

    if !context.is_git_repo {
        return Ok(Json(json!({ "branches": [] })));
    }

    let (stdout, _) = run_git_command(
        &context.repository_root_path,
        &[
            "branch",
            "--list",
            "--format=%(refname:short)%09%(HEAD)%09%(upstream:short)",
        ],
    )
    .await
    .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?;

    let branches = stdout
        .lines()
        .filter_map(parse_branch_line)
        .collect::<Vec<_>>();

    Ok(Json(json!({ "branches": branches })))
}

pub async fn git_log(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;

    if !context.is_git_repo {
        return Ok(Json(json!({ "entries": [] })));
    }

    let (stdout, _) = run_git_command(
        &context.repository_root_path,
        &[
            "log",
            "--date=iso-strict",
            "--pretty=format:%H%x09%h%x09%an%x09%ad%x09%s%x09%b%x00",
            "-n",
            "30",
        ],
    )
    .await
    .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?;

    let entries = stdout
        .split('\x00')
        .filter(|s| !s.trim().is_empty())
        .filter_map(|record| {
            let record = record.trim_start_matches('\n');
            let parts: Vec<&str> = record.splitn(6, '\t').collect();
            if parts.len() < 5 {
                return None;
            }
            Some(GitLogEntry {
                commit: parts[0].to_string(),
                short_commit: parts[1].to_string(),
                author: parts[2].to_string(),
                date: parts[3].to_string(),
                subject: parts[4].to_string(),
                body: parts
                    .get(5)
                    .map(|b| b.trim().to_string())
                    .unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();

    Ok(Json(json!({ "entries": entries })))
}

pub async fn git_diff(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<GitDiffQuery>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;

    if !context.is_git_repo {
        return Ok(Json(json!({
            "path": query.path,
            "staged": query.staged.unwrap_or(false),
            "diff": "",
        })));
    }

    let path = query.path.trim();
    if path.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il percorso del file e' obbligatorio",
        ));
    }
    if path.contains('\0') {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il percorso del file non e' valido",
        ));
    }

    let staged = query.staged.unwrap_or(false);
    let args = if staged {
        vec!["diff", "--staged", "--no-ext-diff", "--", path]
    } else {
        vec!["diff", "--no-ext-diff", "--", path]
    };

    let (stdout, _) = run_git_command(&context.repository_root_path, &args)
        .await
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?;

    Ok(Json(json!({
        "path": path,
        "staged": staged,
        "diff": stdout,
    })))
}

pub async fn git_stage(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<GitPathsRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    execute_git_paths_operation(&state.db, user_id, &context, "stage", &["add"], &body.paths).await
}

pub async fn git_unstage(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<GitPathsRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    execute_git_paths_operation(
        &state.db,
        user_id,
        &context,
        "unstage",
        &["restore", "--staged"],
        &body.paths,
    )
    .await
}

pub async fn git_commit(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<GitCommitRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    if !context.access.can_manage_git {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Non hai permessi Git su questo progetto",
        ));
    }
    if !context.is_git_repo {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il progetto selezionato non e' un repository Git",
        ));
    }
    if body.message.trim().is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il messaggio di commit non puo' essere vuoto",
        ));
    }

    match run_git_command(
        &context.repository_root_path,
        &["commit", "-m", body.message.trim()],
    )
    .await
    {
        Ok((stdout, stderr)) => {
            record_git_operation(
                &state.db,
                user_id,
                &context,
                "commit",
                "success",
                &stdout,
                &stderr,
                json!({ "message": body.message.trim() }),
            )
            .await;
            let git_state = refresh_git_snapshot(&state.db, &context).await?;
            Ok(Json(json!({ "ok": true, "git": git_state })))
        }
        Err(error) => {
            record_git_operation(
                &state.db,
                user_id,
                &context,
                "commit",
                "error",
                "",
                &error.to_string(),
                json!({ "message": body.message.trim() }),
            )
            .await;
            Err(api_error(StatusCode::BAD_REQUEST, error.to_string()))
        }
    }
}

pub async fn git_checkout(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<GitCheckoutRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    if !context.access.can_manage_git {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Non hai permessi Git su questo progetto",
        ));
    }

    match run_git_command(
        &context.repository_root_path,
        &["checkout", body.name.trim()],
    )
    .await
    {
        Ok((stdout, stderr)) => {
            record_git_operation(
                &state.db,
                user_id,
                &context,
                "checkout",
                "success",
                &stdout,
                &stderr,
                json!({ "name": body.name.trim() }),
            )
            .await;
            let git_state = refresh_git_snapshot(&state.db, &context).await?;
            Ok(Json(json!({ "ok": true, "git": git_state })))
        }
        Err(error) => {
            record_git_operation(
                &state.db,
                user_id,
                &context,
                "checkout",
                "error",
                "",
                &error.to_string(),
                json!({ "name": body.name.trim() }),
            )
            .await;
            Err(api_error(StatusCode::BAD_REQUEST, error.to_string()))
        }
    }
}

pub async fn git_create_branch(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<GitCreateBranchRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    if !context.access.can_manage_git {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Non hai permessi Git su questo progetto",
        ));
    }

    match run_git_command(&context.repository_root_path, &["branch", body.name.trim()]).await {
        Ok((stdout, stderr)) => {
            record_git_operation(
                &state.db,
                user_id,
                &context,
                "create_branch",
                "success",
                &stdout,
                &stderr,
                json!({ "name": body.name.trim() }),
            )
            .await;
            Ok(Json(json!({ "ok": true })))
        }
        Err(error) => {
            record_git_operation(
                &state.db,
                user_id,
                &context,
                "create_branch",
                "error",
                "",
                &error.to_string(),
                json!({ "name": body.name.trim() }),
            )
            .await;
            Err(api_error(StatusCode::BAD_REQUEST, error.to_string()))
        }
    }
}

pub async fn git_pull(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<GitRemoteRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    execute_git_remote_operation(&state.db, user_id, &context, "pull", body).await
}

pub async fn git_push(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<GitRemoteRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    execute_git_remote_operation(&state.db, user_id, &context, "push", body).await
}

pub async fn get_git_ui_preferences(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _ = load_project_context(&state.db, project_id, user_id).await?;

    let preferences = sqlx::query_scalar::<_, Value>(
        r#"
        SELECT preferences
        FROM user_project_preferences
        WHERE user_id = $1 AND project_id = $2
        "#,
    )
    .bind(user_id)
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .unwrap_or_else(|| json!({}));

    let show_hunk_map = preferences
        .get("git")
        .and_then(|value| value.get("showHunkMap"))
        .and_then(|value| value.as_bool())
        .unwrap_or(true);

    Ok(Json(json!({
        "showHunkMap": show_hunk_map,
    })))
}

pub async fn update_git_ui_preferences(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<GitUiPreferencesUpdateRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _ = load_project_context(&state.db, project_id, user_id).await?;

    let mut preferences = sqlx::query_scalar::<_, Value>(
        r#"
        SELECT preferences
        FROM user_project_preferences
        WHERE user_id = $1 AND project_id = $2
        "#,
    )
    .bind(user_id)
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .unwrap_or_else(|| json!({}));

    let root = preferences.as_object_mut().ok_or_else(|| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Preferenze progetto non valide",
        )
    })?;

    let git_entry = root.entry("git").or_insert_with(|| json!({}));
    let git_object = git_entry.as_object_mut().ok_or_else(|| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Preferenze Git non valide",
        )
    })?;
    git_object.insert("showHunkMap".to_string(), json!(body.show_hunk_map));

    sqlx::query(
        r#"
        INSERT INTO user_project_preferences (id, user_id, project_id, preferences, created_at, updated_at)
        VALUES (gen_random_uuid(), $1, $2, $3, NOW(), NOW())
        ON CONFLICT (user_id, project_id)
        DO UPDATE SET preferences = EXCLUDED.preferences, updated_at = NOW()
        "#,
    )
    .bind(user_id)
    .bind(project_id)
    .bind(preferences)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({
        "ok": true,
        "showHunkMap": body.show_hunk_map,
    })))
}
