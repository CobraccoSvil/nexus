use super::*;

pub async fn open_project(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    upsert_open_session(
        &state.db,
        user_id,
        &context,
        &[],
        context.details.root_path.as_deref(),
    )
    .await?;
    // list_directory_nodes fa I/O sincrono intensivo (read_dir + metadata per ogni
    // entry + sub-read_dir per has_children). Va eseguito su spawn_blocking per non
    // bloccare il runtime tokio (causa di freeze mcp-core su progetti grandi).
    let root_for_tree = context.root_path.clone();
    let tree = tokio::task::spawn_blocking(move || {
        list_directory_nodes(&root_for_tree, &root_for_tree)
    })
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("spawn_blocking tree: {e}")))?
    ?;
    let git_state = refresh_git_snapshot(&state.db, &context).await?;

    Ok(Json(json!({
        "project": context.details,
        "tree": tree,
        "git": git_state,
    })))
}

pub async fn get_workbench_state(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let preferences = load_user_project_preferences(&state.db, user_id, project_id).await?;
    let workbench = preferences
        .get("workbench")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let session = sqlx::query(
        r#"
        SELECT active_file_paths, terminal_cwd, updated_at
        FROM project_open_sessions
        WHERE user_id = $1 AND project_id = $2
        "#,
    )
    .bind(user_id)
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let active_file_paths = session
        .as_ref()
        .and_then(|row| row.try_get::<Option<Value>, _>("active_file_paths").ok().flatten())
        .unwrap_or_else(|| json!([]));
    let terminal_cwd = session
        .as_ref()
        .and_then(|row| row.try_get::<Option<String>, _>("terminal_cwd").ok().flatten())
        .or_else(|| context.details.root_path.clone());
    let updated_at = session
        .as_ref()
        .and_then(|row| {
            row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("updated_at")
                .ok()
                .flatten()
        })
        .map(|value| value.to_rfc3339());

    Ok(Json(json!({
        "state": workbench,
        "session": {
            "activeFilePaths": active_file_paths,
            "terminalCwd": terminal_cwd,
            "updatedAt": updated_at,
        }
    })))
}

pub async fn update_workbench_state(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<WorkbenchStateUpdateRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;

    let mut preferences = load_user_project_preferences(&state.db, user_id, project_id).await?;
    let root = preferences.as_object_mut().ok_or_else(|| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Preferenze progetto non valide",
        )
    })?;
    root.insert("workbench".to_string(), body.state.clone());
    save_user_project_preferences(&state.db, user_id, project_id, preferences).await?;

    upsert_open_session(
        &state.db,
        user_id,
        &context,
        body.active_file_paths.as_deref().unwrap_or(&[]),
        body.terminal_cwd
            .as_deref()
            .or(context.details.root_path.as_deref()),
    )
    .await?;

    Ok(Json(json!({
        "ok": true,
        "state": body.state,
    })))
}

pub async fn create_terminal_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;

    if !context.access.can_write {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Non hai permessi di scrittura per aprire un terminale su questo progetto",
        ));
    }

    let session_id = Uuid::new_v4().to_string();
    let shell = terminal_shell();
    let expires_at = SystemTime::now()
        .checked_add(Duration::from_secs(60 * 15))
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .ok_or_else(|| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Impossibile creare la sessione terminale",
            )
        })?;

    let cwd = context.root_path.to_string_lossy().to_string();
    let admin_root = load_projects_base_root(&state.db).await?;
    let root = admin_root.to_string_lossy().to_string();
    let claims_payload = TerminalSessionClaims {
        sid: &session_id,
        uid: &user_id.to_string(),
        pid: &context.project_id.to_string(),
        root: &root,
        cwd: &cwd,
        shell: &shell,
        exp: expires_at,
    };
    let payload_json = serde_json::to_vec(&claims_payload).map_err(|_| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Impossibile serializzare la sessione terminale",
        )
    })?;
    let payload_base64 = URL_SAFE_NO_PAD.encode(payload_json);
    let secret = terminal_session_secret(&state.db).await;
    let signature = sign_terminal_token(&payload_base64, &secret);
    let token = format!("{payload_base64}.{signature}");

    Ok(Json(json!(TerminalSessionResponse {
        session_id,
        token,
        working_directory: cwd,
        shell,
        expires_at,
    })))
}
