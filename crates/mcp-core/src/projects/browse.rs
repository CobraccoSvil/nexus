// Navigazione filesystem server: browse directory e creazione directory.

use super::*;

pub async fn browse_server_directories(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<FsBrowseQuery>,
) -> ApiResult {
    let _ = parse_user_id(&claims)?;
    let (current, base_root) = resolve_browse_path(&state.db, query.path.as_deref()).await?;
    let parent_path = current.parent().and_then(|parent| {
        if path_within(&base_root, parent) || extra_roots_allowed(parent) {
            Some(parent.to_string_lossy().to_string())
        } else {
            None
        }
    });

    // Costruisce lista radici: base_root + eventuali NEXUS_EXTRA_ROOTS
    let mut roots = vec![base_root.to_string_lossy().to_string()];
    if let Ok(extra) = std::env::var("NEXUS_EXTRA_ROOTS") {
        for raw in extra.split(',') {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                if let Ok(canonical) = PathBuf::from(trimmed).canonicalize() {
                    let s = canonical.to_string_lossy().to_string();
                    if !roots.contains(&s) {
                        roots.push(s);
                    }
                }
            }
        }
    }

    Ok(Json(json!({
        "roots": roots,
        "currentPath": current.to_string_lossy().to_string(),
        "parentPath": parent_path,
        "directories": list_browse_directories(&current),
    })))
}

pub async fn create_server_directory(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<CreateDirectoryRequest>,
) -> ApiResult {
    let _ = parse_user_id(&claims)?;
    let (parent, _) = resolve_browse_path(&state.db, Some(body.parent_path.as_str())).await?;
    let dir_name = validate_directory_name(&body.name)?;
    let target = parent.join(dir_name);

    if target.exists() {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Esiste gia' una directory con questo nome",
        ));
    }

    fs::create_dir(&target)
        .await
        .map_err(map_create_dir_error)?;

    Ok(Json(json!({
        "ok": true,
        "path": target.to_string_lossy().to_string(),
    })))
}
