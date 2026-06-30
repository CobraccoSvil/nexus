// Funzionalita' di clone progetto da URL git.

use super::*;

/// GET /api/projects/clone-target-exists?repo=<name>
/// Ritorna { "exists": bool, "path": string } per projects_base_root/<name>
pub async fn clone_target_exists(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    let _ = parse_user_id(&claims)?;
    let repo = params
        .get("repo")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if repo.is_empty() {
        return Ok(Json(json!({ "exists": false, "path": "" })));
    }
    let base_root = load_projects_base_root(&state.db).await?;
    let target = base_root.join(&repo);
    let exists = target.exists();
    Ok(Json(json!({
        "exists": exists,
        "path": target.to_string_lossy(),
    })))
}

/// POST /api/projects/clone  { "url": "https://github.com/...", "name": "optional" }
/// Clona il repository in projects_base_root, poi lo registra come progetto.
pub async fn clone_project(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;

    let url = body
        .get("url")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Campo 'url' obbligatorio"))?;

    // Ricava il nome directory dall'URL (ultimo segmento senza .git)
    let dir_name_from_url = url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("repo")
        .trim_end_matches(".git")
        .to_string();
    let dir_name = dir_name_from_url
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect::<String>();
    let dir_name = if dir_name.is_empty() {
        "repo".to_string()
    } else {
        dir_name
    };

    let base_root = load_projects_base_root(&state.db).await?;
    let dest = base_root.join(&dir_name);

    if dest.exists() {
        // Esiste gia': registra direttamente
        let register_body = serde_json::json!({
            "absolute_path": dest.to_string_lossy(),
            "name": body.get("name").and_then(|v| v.as_str()).unwrap_or(&dir_name),
        });
        let register_req = Json(
            serde_json::from_value::<RegisterProjectRequest>(register_body)
                .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?,
        );
        return register_project(State(state), Extension(claims), register_req).await;
    }

    // Per URL HTTPS GitHub, inietta il token OAuth per repo private.
    let clone_url =
        if url.starts_with("https://github.com/") || url.starts_with("https://www.github.com/") {
            match crate::github::ensure_github_authorized_user(&state.db, user_id).await {
                Ok(Some(authorized)) => {
                    let bare = url
                        .trim_start_matches("https://")
                        .trim_start_matches("www.");
                    format!("https://{}@{}", authorized.access_token, bare)
                }
                _ => url.clone(),
            }
        } else {
            url.clone()
        };

    // Esegue git clone
    let output = tokio::process::Command::new("git")
        .arg("clone")
        .arg("--depth=1")
        .arg(&clone_url)
        .arg(&dest)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "/bin/echo")
        .output()
        .await
        .map_err(|e| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("git clone fallito: {e}"),
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Rimuove il token dall'errore prima di restituirlo al client
        let clean_stderr = if clone_url != url {
            stderr.replace(&clone_url, &url)
        } else {
            stderr.to_string()
        };
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            format!("git clone fallito: {clean_stderr}"),
        ));
    }

    let project_name = body
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&dir_name)
        .to_string();

    let register_body = RegisterProjectRequest {
        absolute_path: dest.to_string_lossy().to_string(),
        name: Some(project_name),
    };
    register_project(State(state), Extension(claims), Json(register_body)).await
}
