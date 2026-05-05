use super::*;

// ── GET /api/projects/:id/changes?since=<unix_ms> ────────────────────────────
/// Restituisce i file del progetto modificati dopo `since` (timestamp Unix in
/// millisecondi). Esclude le directory di build/cache. Limite 200 file.
/// Usato dal frontend per il banner "N file modificati - Riavvia tutti".
pub async fn get_project_changes(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let root = context.root_path.to_string_lossy().to_string();

    let since_ms: u64 = q.get("since").and_then(|v| v.parse().ok()).unwrap_or(0);
    let since = std::time::UNIX_EPOCH + std::time::Duration::from_millis(since_ms);

    // BFS iterativa, salta dir di build/cache, limite hard di 200 file
    const SKIP_DIRS: &[&str] = &[
        ".git", "node_modules", ".next", ".turbo", ".cache", "__pycache__",
        ".venv", "venv", "obj", "bin", ".terraform", "vendor", ".dotnet",
        "dist", "build", "out", "target", ".nuxt", ".svelte-kit", ".parcel-cache",
        "playwright-report", "test-results", ".pytest_cache", ".mypy_cache",
        ".tsbuildinfo",
    ];

    let mut changed: Vec<serde_json::Value> = Vec::new();
    let mut queue: std::collections::VecDeque<std::path::PathBuf> = std::collections::VecDeque::new();
    queue.push_back(std::path::PathBuf::from(&root));

    'outer: while let Some(dir) = queue.pop_front() {
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(r) => r,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name();
            let name_s = name.to_string_lossy().to_string();
            if name_s.starts_with('.') && name_s != ".env" && name_s != ".env.local" {
                continue;
            }
            let ftype = match entry.file_type().await {
                Ok(t) => t,
                Err(_) => continue,
            };
            let path = entry.path();
            if ftype.is_dir() {
                if SKIP_DIRS.contains(&name_s.as_str()) { continue; }
                queue.push_back(path);
            } else if ftype.is_file() {
                if let Ok(meta) = entry.metadata().await {
                    if let Ok(mtime) = meta.modified() {
                        if mtime > since {
                            let mtime_ms = mtime.duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64).unwrap_or(0);
                            let rel = path.strip_prefix(&root)
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_else(|_| path.to_string_lossy().to_string());
                            changed.push(json!({ "path": rel, "mtime": mtime_ms }));
                            if changed.len() >= 200 { break 'outer; }
                        }
                    }
                }
            }
        }
    }

    // Ordina per mtime decrescente
    changed.sort_by_key(|v| std::cmp::Reverse(v["mtime"].as_u64().unwrap_or(0)));

    Ok(Json(json!({
        "since": since_ms,
        "count": changed.len(),
        "changed": changed,
    })))
}
