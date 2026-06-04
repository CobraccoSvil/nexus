// Analisi AI profonda del progetto tramite agent.project.analyzer.

use super::*;

// ── Costanti deep-analyzer ────────────────────────────────────────────────────

/// Lista (allowlist) dei file di configurazione raccolti dal deep-analyzer.
const DEEP_ANALYZER_CONFIG_PATTERNS: &[&str] = &[
    ".env",
    ".env.example",
    ".env.dev",
    ".env.development",
    ".env.local",
    ".env.prod",
    ".env.prod.example",
    "docker-compose.yml",
    "docker-compose.dev.yml",
    "docker-compose.prod.yml",
    "Dockerfile",
    "package.json",
    "pyproject.toml",
    "Cargo.toml",
    "go.mod",
    "Gemfile",
    "composer.json",
    "appsettings.json",
    "appsettings.Development.json",
    "appsettings.Production.json",
    "Makefile",
    "README.md",
];

/// Massimo numero di file di config raccolti.
const DEEP_ANALYZER_MAX_FILES: usize = 20;
/// Massima dimensione singolo file (byte) — oltre viene troncato.
const DEEP_ANALYZER_MAX_FILE_BYTES: usize = 12_000;
/// Profondita' massima di walk per cercare file di config nelle subdirectory.
const DEEP_ANALYZER_MAX_DEPTH: usize = 3;

// ── Helper privati ────────────────────────────────────────────────────────────

/// Walk asincrona limitata: raccoglie file il cui basename matcha i pattern.
pub(super) async fn collect_config_files(root: &Path) -> Vec<serde_json::Value> {
    let patterns: std::collections::HashSet<&str> =
        DEEP_ANALYZER_CONFIG_PATTERNS.iter().copied().collect();
    let mut found: Vec<serde_json::Value> = Vec::new();
    let mut stack: Vec<(std::path::PathBuf, usize)> = vec![(root.to_path_buf(), 0)];

    let skip_dirs: &[&str] = &[
        ".git",
        "node_modules",
        "target",
        "dist",
        "build",
        ".next",
        "bin",
        "obj",
        ".venv",
        "__pycache__",
        ".turbo",
        ".cache",
        "vendor",
    ];

    while let Some((dir, depth)) = stack.pop() {
        if found.len() >= DEEP_ANALYZER_MAX_FILES {
            break;
        }
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            if let Ok(ft) = entry.file_type().await {
                if ft.is_dir() {
                    if depth + 1 > DEEP_ANALYZER_MAX_DEPTH {
                        continue;
                    }
                    if name.starts_with('.') && name != "." && name != ".github" {
                        continue;
                    }
                    if skip_dirs.contains(&name.as_str()) {
                        continue;
                    }
                    stack.push((path, depth + 1));
                } else if ft.is_file() && patterns.contains(name.as_str()) {
                    if found.len() >= DEEP_ANALYZER_MAX_FILES {
                        break;
                    }
                    let rel_path = path
                        .strip_prefix(root)
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| name.clone());
                    let raw = tokio::fs::read(&path).await.unwrap_or_default();
                    let truncated = raw.len() > DEEP_ANALYZER_MAX_FILE_BYTES;
                    let bytes_slice = if truncated {
                        &raw[..DEEP_ANALYZER_MAX_FILE_BYTES]
                    } else {
                        &raw[..]
                    };
                    let content = String::from_utf8_lossy(bytes_slice).to_string();
                    found.push(json!({
                        "path": rel_path,
                        "content": content,
                        "truncated": truncated,
                        "size_bytes": raw.len(),
                    }));
                }
            }
        }
    }
    found
}

/// Recupera i servizi systemd registrati per il progetto.
pub(super) async fn collect_registered_services(slug: &str) -> Vec<serde_json::Value> {
    use tokio::process::Command;
    let out = Command::new("bash")
        .arg("-lc")
        .arg(format!(
            "systemctl --user list-unit-files '{slug}-*.service' --no-pager --no-legend 2>/dev/null | awk '{{print $1}}'"
        ))
        .output()
        .await;
    let mut services = Vec::new();
    if let Ok(o) = out {
        let txt = String::from_utf8_lossy(&o.stdout);
        for line in txt.lines() {
            let unit = line.trim();
            if unit.is_empty() {
                continue;
            }
            let info = Command::new("bash")
                .arg("-lc")
                .arg(format!(
                    "systemctl --user show '{unit}' --property=ActiveState,ExecStart,WorkingDirectory --no-pager 2>/dev/null"
                ))
                .output().await;
            let mut active_state = String::new();
            let mut exec_start = String::new();
            let mut workdir = String::new();
            if let Ok(i) = info {
                let body = String::from_utf8_lossy(&i.stdout);
                for ln in body.lines() {
                    if let Some(v) = ln.strip_prefix("ActiveState=") {
                        active_state = v.to_string();
                    } else if let Some(v) = ln.strip_prefix("ExecStart=") {
                        exec_start = v.to_string();
                    } else if let Some(v) = ln.strip_prefix("WorkingDirectory=") {
                        workdir = v.to_string();
                    }
                }
            }
            services.push(json!({
                "unit": unit,
                "active_state": active_state,
                "exec_start": exec_start,
                "working_dir": workdir,
            }));
        }
    }
    services
}

// ── Handler HTTP ──────────────────────────────────────────────────────────────

/// POST /api/projects/:id/deep-analyze — analisi AI profonda del progetto.
///
/// Refactor 0102 (Wave structural): handler ASINCRONO. Spezzato in:
///  - Fase sync (rapida): insert riga insights con status='running', return 202 + run_id
///  - Fase async (background tokio task): chiamata brain + UPDATE riga finale
///
/// Risolve il bug "deep-analyze 500/timeout proxy 30s": la chiamata sincrona
/// poteva durare oltre il timeout del proxy Next.js, dropping connection,
/// client riceveva 500 anche se il job era in corso server-side.
///
/// Il client ora chiama POST → riceve 202 + run_id, poi GET /insights polla
/// finche' status != 'running'.
pub async fn deep_analyze_project(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    let context = load_project_context(&state.db, project_id, user_id).await?;
    let root = context.repository_root_path.clone();
    if !root.is_dir() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Directory del progetto non trovata",
        ));
    }

    // ── Fase sync: insert riga 'running' e return 202 ────────────────────────
    let run_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO nexus_project_insights
            (project_id, insight_version, insights, prompt_key, prompt_version,
             status, config_files_count)
         VALUES ($1, 1, '{}'::jsonb, 'agent.project.analyzer', 1, 'running', 0)
         RETURNING id",
    )
    .bind(project_id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("insert running: {e}"),
        )
    })?;

    // Snapshot dei dati per la fase async (la closure vive con 'static).
    let db = state.db.clone();
    let neural = state.orchestrator.neural.clone();
    let project_channels = state.project_channels.clone();
    let repo_root_str = root.to_string_lossy().to_string();
    let project_name = context.details.name.clone();
    let project_slug = context.details.slug.clone();

    tokio::spawn(async move {
        let started = std::time::Instant::now();

        // 1. Recupera l'ultima analisi statica
        let static_analysis: serde_json::Value =
            sqlx::query_scalar::<_, Option<serde_json::Value>>(
                "SELECT analysis_json FROM projects WHERE id = $1",
            )
            .bind(project_id)
            .fetch_optional(&db)
            .await
            .ok()
            .flatten()
            .flatten()
            .unwrap_or(json!({}));

        let lang_hint = static_analysis
            .get("languages")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|first| first.get("language"))
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let frameworks_list: Vec<String> = static_analysis
            .get("frameworks")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let repo_summary = format!(
            "{} file totali in {} (linguaggio dominante: {}). Framework: {}.",
            static_analysis
                .get("totalFiles")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            project_name,
            if lang_hint.is_empty() {
                "non determinato"
            } else {
                lang_hint.as_str()
            },
            if frameworks_list.is_empty() {
                "nessuno".to_string()
            } else {
                frameworks_list.join(", ")
            },
        );

        // 2. Raccoglie config files dal filesystem
        let config_files = collect_config_files(&root).await;
        let cfg_count = config_files.len() as i32;

        // 3. Servizi systemd registrati
        let services = collect_registered_services(&project_slug).await;

        // 4. Chiama il brain (timeout 5 minuti — dato che siamo in background, non
        //    proxy timeout, possiamo essere generosi)
        let brain_url =
            std::env::var("BRAIN_REST_URL").unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());
        let body = json!({
            "project_id": project_id.to_string(),
            "project_name": project_name,
            "repo_summary": repo_summary,
            "lang_hint": lang_hint,
            "frameworks_list": frameworks_list,
            "config_files": config_files,
            "registered_services": services,
            "provider_chain": [],
        });

        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = mark_failed(&db, run_id, &format!("client build: {e}"), 0).await;
                return;
            }
        };
        let response = match client
            .post(format!(
                "{}/agent/project-analyze",
                brain_url.trim_end_matches('/')
            ))
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let _ = mark_failed(
                    &db,
                    run_id,
                    &format!("brain unreachable: {e}"),
                    started.elapsed().as_millis() as i32,
                )
                .await;
                return;
            }
        };

        if !response.status().is_success() {
            let st = response.status();
            let txt = response.text().await.unwrap_or_default();
            let _ = mark_failed(
                &db,
                run_id,
                &format!("brain error {st}: {}", &txt[..txt.len().min(300)]),
                started.elapsed().as_millis() as i32,
            )
            .await;
            return;
        }

        let brain_resp: serde_json::Value = match response.json().await {
            Ok(v) => v,
            Err(e) => {
                let _ = mark_failed(
                    &db,
                    run_id,
                    &format!("brain json: {e}"),
                    started.elapsed().as_millis() as i32,
                )
                .await;
                return;
            }
        };

        // 5. UPDATE finale della riga 'running'
        let status_str = brain_resp
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("failed");
        let model_used = brain_resp
            .get("model_used")
            .and_then(|v| v.as_str())
            .map(String::from);
        let duration_ms = brain_resp
            .get("duration_ms")
            .and_then(|v| v.as_i64())
            .unwrap_or_else(|| started.elapsed().as_millis() as i64)
            as i32;
        let insights_payload = brain_resp.get("insights").cloned().unwrap_or(json!({}));
        let error_msg = brain_resp
            .get("error")
            .and_then(|v| v.as_str())
            .map(String::from);

        let _ = sqlx::query(
            "UPDATE nexus_project_insights
                SET insights = $1, model_used = $2, duration_ms = $3,
                    config_files_count = $4, status = $5, error_message = $6
                WHERE id = $7",
        )
        .bind(&insights_payload)
        .bind(&model_used)
        .bind(duration_ms)
        .bind(cfg_count)
        .bind(status_str)
        .bind(&error_msg)
        .bind(run_id)
        .execute(&db)
        .await;

        tracing::info!(
            "deep_analyze background: run_id={} status={} duration_ms={}",
            run_id,
            status_str,
            duration_ms
        );

        // Se l'analisi e' completata con successo, popola la Knowledge Base
        if status_str == "completed"
            && !insights_payload
                .as_object()
                .map(|o| o.is_empty())
                .unwrap_or(true)
        {
            crate::knowledge::seed_knowledge_from_insights(
                db,
                neural,
                project_id,
                insights_payload,
                Some(repo_root_str),
                project_channels,
            )
            .await;
        }
    });

    // Risposta immediata 202 Accepted con run_id per polling client-side
    Ok(Json(json!({
        "run_id": run_id,
        "status": "running",
        "message": "Analisi avviata in background. Polla GET /api/projects/:id/insights ogni 3s finche' status != 'running'.",
    })))
}

/// Helper: marca una riga insights come 'failed' con error_message.
async fn mark_failed(
    db: &sqlx::PgPool,
    run_id: i64,
    msg: &str,
    duration_ms: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE nexus_project_insights
            SET status = 'failed', error_message = $1, duration_ms = $2
            WHERE id = $3",
    )
    .bind(msg)
    .bind(duration_ms)
    .bind(run_id)
    .execute(db)
    .await?;
    tracing::warn!("deep_analyze background: run_id={} FAILED: {}", run_id, msg);
    Ok(())
}

/// GET /api/projects/:id/insights — ritorna l'ultimo insight salvato.
pub async fn get_project_insights(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    // Verifica accesso
    let _ = load_project_context(&state.db, project_id, user_id).await?;

    let row = sqlx::query(
        "SELECT insights, model_used, duration_ms, config_files_count, status,
                error_message, created_at
         FROM nexus_project_insights
         WHERE project_id = $1
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("db: {e}")))?;

    match row {
        Some(r) => {
            let insights: serde_json::Value = r.try_get("insights").unwrap_or(json!({}));
            let model_used: Option<String> = r.try_get("model_used").ok();
            let duration_ms: Option<i32> = r.try_get("duration_ms").ok();
            let config_files_count: Option<i32> = r.try_get("config_files_count").ok();
            let status: Option<String> = r.try_get("status").ok();
            let error_message: Option<String> = r.try_get("error_message").ok();
            let created_at: Option<chrono::DateTime<chrono::Utc>> = r.try_get("created_at").ok();
            Ok(Json(json!({
                "exists": true,
                "insights": insights,
                "model_used": model_used,
                "duration_ms": duration_ms,
                "config_files_count": config_files_count,
                "status": status,
                "error_message": error_message,
                "created_at": created_at,
            })))
        }
        None => Ok(Json(json!({ "exists": false }))),
    }
}
