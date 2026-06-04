//! Fix M14: endpoint REST per browser-check (smoke E2E con cattura console errors).
//!
//! Wrapper minimale su test_playwright + smoke.spec.ts (gia creato da install-playwright Fix M19).
//! L'agente puo chiamare questo endpoint per verificare che il frontend renda senza
//! errori JS al primo render, anche per pagine raggiungibili solo via auth (login OK,
//! ma /cars potrebbe avere bug runtime non rilevabili via curl).
//!
//! POST /api/projects/:id/services/browser-check
//! Body: {target_dir?: string, base_url?: string, route?: string}
//!
//! Output: {ok, route_checked, console_errors, page_url, stdout_tail, stderr_tail}

use super::*;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const BROWSER_CHECK_TIMEOUT_SECS: u64 = 120;

pub async fn browser_check(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;

    if !context.access.can_write {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Non hai permessi su questo progetto",
        ));
    }

    let target_dir_arg = body.get("target_dir").and_then(Value::as_str);
    let route = body
        .get("route")
        .and_then(Value::as_str)
        .unwrap_or("/")
        .to_string();
    let explicit_base_url = body.get("base_url").and_then(Value::as_str);

    // Detect target dir (riusa la logica di playwright_install)
    let target_dir: PathBuf = if let Some(t) = target_dir_arg {
        let p = context.root_path.join(t);
        if !p.starts_with(&context.root_path) {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "target_dir fuori dalla root del progetto",
            ));
        }
        p
    } else {
        // Default: cerca frontend/ poi root
        let candidates = ["frontend", "client", "web", "app", "ui"];
        let mut found = context.root_path.clone();
        for c in &candidates {
            let p = context.root_path.join(c);
            if p.join("playwright.config.ts").is_file() || p.join("playwright.config.js").is_file()
            {
                found = p;
                break;
            }
        }
        found
    };

    if !target_dir.join("playwright.config.ts").is_file()
        && !target_dir.join("playwright.config.js").is_file()
    {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            format!(
                "playwright.config non trovato in {} — usa POST /services/install-playwright prima",
                target_dir.display()
            ),
        ));
    }

    // Determina BASE_URL: explicit > legge nexus_port_allocations
    let base_url = if let Some(u) = explicit_base_url {
        u.to_string()
    } else {
        let port_rows =
            sqlx::query("SELECT port, label FROM nexus_port_allocations WHERE project_id=$1")
                .bind(project_id)
                .fetch_all(&state.db)
                .await
                .map_err(|e| {
                    api_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("DB query ports: {}", e),
                    )
                })?;

        // pick_dev_port semplificato
        let dev_kw = ["dev", "app", "http", "web", "frontend", "vite", "next"];
        let backend_kw = ["backend", "api", "fastify", "express"];
        let mut chosen: i32 = 5173;
        for kw in &dev_kw {
            if let Some(row) = port_rows.iter().find(|r| {
                let l: String = r.get("label");
                let lc = l.to_lowercase();
                lc.contains(kw) && !backend_kw.iter().any(|bk| lc.contains(bk))
            }) {
                chosen = row.get("port");
                break;
            }
        }
        format!("http://localhost:{}", chosen)
    };

    // Esegui solo smoke.spec.ts con BASE_URL override
    let mut cmd = Command::new("npx");
    cmd.args([
        "playwright",
        "test",
        "e2e/smoke.spec.ts",
        "--reporter=line",
        "--workers=1",
    ])
    .env("BASE_URL", &base_url)
    .env("PLAYWRIGHT_BASE_URL", &base_url)
    .current_dir(&target_dir)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("spawn playwright: {}", e),
        )
    })?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let exit_status = tokio::time::timeout(
        std::time::Duration::from_secs(BROWSER_CHECK_TIMEOUT_SECS),
        child.wait(),
    )
    .await
    .map_err(|_| {
        api_error(
            StatusCode::REQUEST_TIMEOUT,
            format!(
                "timeout {}s eseguendo browser-check",
                BROWSER_CHECK_TIMEOUT_SECS
            ),
        )
    })?
    .map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("wait playwright: {}", e),
        )
    })?;

    let mut stdout_buf = String::new();
    let mut stderr_buf = String::new();
    if let Some(mut s) = stdout {
        let _ = s.read_to_string(&mut stdout_buf).await;
    }
    if let Some(mut s) = stderr {
        let _ = s.read_to_string(&mut stderr_buf).await;
    }

    // Parser semplice degli errori console nello stdout (smoke test stampa li)
    let console_errors: Vec<String> = stdout_buf
        .lines()
        .filter(|l| {
            let lc = l.to_lowercase();
            lc.contains("error")
                || lc.contains("syntaxerror")
                || lc.contains("typeerror")
                || lc.contains("referenceerror")
        })
        .take(20)
        .map(|s| s.trim().to_string())
        .collect();

    let exit_code = exit_status.code().unwrap_or(-1);
    let ok = exit_code == 0 && console_errors.is_empty();

    let stdout_lines: Vec<&str> = stdout_buf.lines().collect();
    let stdout_start = stdout_lines.len().saturating_sub(30);
    let stdout_tail = stdout_lines[stdout_start..].join("\n");

    let stderr_lines: Vec<&str> = stderr_buf.lines().collect();
    let stderr_start = stderr_lines.len().saturating_sub(20);
    let stderr_tail = stderr_lines[stderr_start..].join("\n");

    let target_rel = target_dir
        .strip_prefix(&context.root_path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    Ok(Json(json!({
        "ok": ok,
        "exit_code": exit_code,
        "route_checked": route,
        "base_url": base_url,
        "page_url": format!("{}{}", base_url.trim_end_matches('/'), route),
        "console_errors": console_errors,
        "stdout_tail": stdout_tail,
        "stderr_tail": stderr_tail,
        "target_dir": target_rel,
    })))
}
