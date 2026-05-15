//! Fix M41: endpoint per riscrivere file di config con le porte allocate.
//!
//! POST /api/projects/:id/services/sync-ports-to-files
//!
//! L'agente puo' aver generato file con porte hardcoded (vite.config.ts
//! `server.port = 5173`, vite proxy `target: 'http://localhost:3002'`).
//! Nexus alloca porte dinamiche in `nexus_port_allocations` (es. 34137/34148)
//! ma non riscrive automaticamente i sorgenti.
//!
//! Questo endpoint:
//! 1. Legge nexus_port_allocations per il progetto
//! 2. Per ogni file vite.config.{ts,js,mjs}:
//!    - Sostituisce `port: <num>` con `port: parseInt(process.env.PORT ?? '<allocated_frontend>')`
//!    - Sostituisce `'http://localhost:<num>'` in `target:` con `<allocated_backend>`
//! 3. Scrive .env nel backend con `PORT=<allocated_backend>` se assente
//! 4. Ritorna un report di file modificati / patch applicati
//!
//! NB: best-effort, regex-based. Non un AST refactor. Pensato per chiudere
//! la divergenza M40 sui progetti generati pre-migration 0141.

use super::*;
use regex::Regex;

pub async fn sync_ports_to_files(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let root = &context.root_path;

    // 1. Carica allocazioni in DB
    let allocations: Vec<(i32, String)> =
        sqlx::query_as::<_, (i32, String)>(
            "SELECT port, COALESCE(label, '') FROM nexus_port_allocations \
             WHERE project_id = $1",
        )
        .bind(project_id)
        .fetch_all(&state.db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut backend_port: Option<i32> = None;
    let mut frontend_port: Option<i32> = None;
    for (port, label) in &allocations {
        let lbl = label.to_lowercase();
        if lbl.contains("backend") || lbl.contains("api") {
            backend_port.get_or_insert(*port);
        }
        if lbl.contains("frontend") || lbl.contains("web") || lbl.contains("dev") && lbl != "backend-dev" {
            frontend_port.get_or_insert(*port);
        }
    }

    let mut patches = Vec::new();

    // 2. Patch vite.config.{ts,js,mjs} (frontend)
    if let Some(fp) = frontend_port {
        for ext in &["ts", "js", "mjs"] {
            let path = root.join("frontend").join(format!("vite.config.{}", ext));
            if !path.is_file() {
                continue;
            }
            let content = match tokio::fs::read_to_string(&path).await {
                Ok(c) => c,
                Err(_) => continue,
            };
            let mut new_content = content.clone();

            // server.port -> parseInt(process.env.PORT ?? '<fp>')
            let port_re = Regex::new(r"port\s*:\s*\d{2,5}").unwrap();
            new_content = port_re
                .replace(&new_content, format!(
                    "port: parseInt(process.env.PORT ?? '{}')",
                    fp
                ))
                .to_string();

            // proxy target -> backend allocated
            if let Some(bp) = backend_port {
                let proxy_re = Regex::new(
                    r#"target\s*:\s*['"]http://localhost:\d{2,5}['"]"#,
                )
                .unwrap();
                new_content = proxy_re
                    .replace(
                        &new_content,
                        format!("target: 'http://localhost:{}'", bp),
                    )
                    .to_string();
            }

            if new_content != content {
                if let Err(e) = tokio::fs::write(&path, &new_content).await {
                    tracing::warn!("sync_ports: write {} fallito: {}", path.display(), e);
                    continue;
                }
                patches.push(json!({
                    "file": format!("frontend/vite.config.{}", ext),
                    "frontend_port": fp,
                    "backend_port": backend_port,
                }));
            }
        }
    }

    // 3. Backend .env con PORT=backend_port
    if let Some(bp) = backend_port {
        let env_path = root.join("backend").join(".env");
        let existing = tokio::fs::read_to_string(&env_path).await.unwrap_or_default();
        let port_line = format!("PORT={}", bp);
        let new_env = if existing.contains("PORT=") {
            let re = Regex::new(r"(?m)^PORT=.*$").unwrap();
            re.replace(&existing, port_line.as_str()).to_string()
        } else if existing.is_empty() {
            port_line.clone()
        } else {
            format!("{}\n{}", existing.trim_end(), port_line)
        };
        if new_env != existing {
            if let Err(e) = tokio::fs::write(&env_path, &new_env).await {
                tracing::warn!("sync_ports: write backend/.env fallito: {}", e);
            } else {
                patches.push(json!({
                    "file": "backend/.env",
                    "added_or_updated": "PORT",
                    "value": bp,
                }));
            }
        }
    }

    // 4. playwright.config.ts baseURL
    if let Some(fp) = frontend_port {
        for cfg in &["playwright.config.ts", "playwright.config.js", "frontend/playwright.config.ts"] {
            let path = root.join(cfg);
            if !path.is_file() {
                continue;
            }
            let content = match tokio::fs::read_to_string(&path).await {
                Ok(c) => c,
                Err(_) => continue,
            };
            let baseurl_re = Regex::new(
                r#"baseURL\s*:\s*['"]http://localhost:\d{2,5}['"]"#,
            )
            .unwrap();
            let new_content = baseurl_re
                .replace(
                    &content,
                    format!("baseURL: 'http://localhost:{}'", fp),
                )
                .to_string();
            if new_content != content {
                if let Err(e) = tokio::fs::write(&path, &new_content).await {
                    tracing::warn!("sync_ports: write {} fallito: {}", path.display(), e);
                    continue;
                }
                patches.push(json!({
                    "file": cfg,
                    "baseURL_port": fp,
                }));
            }
        }
    }

    Ok(Json(json!({
        "ok": true,
        "backend_port": backend_port,
        "frontend_port": frontend_port,
        "patches_applied": patches.len(),
        "patches": patches,
    })))
}
