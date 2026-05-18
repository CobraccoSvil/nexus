use super::*;

pub async fn get_project_problems(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    let mut items = Vec::<Value>::new();

    let quality_rows = sqlx::query(
        r#"
        SELECT id, file_path, category, severity, finding, created_at
        FROM quality_findings
        WHERE project_id = $1
        ORDER BY created_at DESC
        LIMIT 100
        "#,
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    for row in quality_rows {
        let finding = row.get::<Value, _>("finding");
        items.push(json!({
            "id": row.get::<Uuid, _>("id").to_string(),
            "severity": row.get::<String, _>("severity"),
            "source": row.get::<String, _>("category"),
            "message": finding.get("message")
                .and_then(Value::as_str)
                .or_else(|| finding.get("title").and_then(Value::as_str))
                .unwrap_or("Quality finding"),
            "filePath": row.get::<String, _>("file_path"),
            "line": finding.get("line").and_then(Value::as_i64),
            "column": finding.get("column").and_then(Value::as_i64),
            "createdAt": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
        }));
    }

    let security_rows = sqlx::query(
        r#"
        SELECT id, file_path, severity, finding, created_at
        FROM security_findings
        WHERE project_id = $1
        ORDER BY created_at DESC
        LIMIT 100
        "#,
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    for row in security_rows {
        let finding = row.get::<Value, _>("finding");
        items.push(json!({
            "id": row.get::<Uuid, _>("id").to_string(),
            "severity": row.get::<String, _>("severity"),
            "source": "security",
            "message": finding.get("message")
                .and_then(Value::as_str)
                .or_else(|| finding.get("title").and_then(Value::as_str))
                .unwrap_or("Security finding"),
            "filePath": row.get::<String, _>("file_path"),
            "line": finding.get("line").and_then(Value::as_i64),
            "column": finding.get("column").and_then(Value::as_i64),
            "createdAt": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
        }));
    }

    let failed_jobs = sqlx::query(
        r#"
        SELECT id, kind, status, input, created_at
        FROM jobs
        WHERE project_id = $1 AND status NOT IN ('queued', 'running', 'completed', 'success')
        ORDER BY created_at DESC
        LIMIT 50
        "#,
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    for row in failed_jobs {
        let input = row.get::<Value, _>("input");
        items.push(json!({
            "id": row.get::<Uuid, _>("id").to_string(),
            "severity": "error",
            "source": row.get::<String, _>("kind"),
            "message": input.get("message").and_then(Value::as_str).unwrap_or("Job fallito"),
            "filePath": input.get("file_path").and_then(Value::as_str),
            "line": input.get("line").and_then(Value::as_i64),
            "column": input.get("column").and_then(Value::as_i64),
            "createdAt": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
        }));
    }

    items.sort_by(|left, right| {
        let left_severity = left
            .get("severity")
            .and_then(Value::as_str)
            .map(severity_rank)
            .unwrap_or(2);
        let right_severity = right
            .get("severity")
            .and_then(Value::as_str)
            .map(severity_rank)
            .unwrap_or(2);
        left_severity.cmp(&right_severity).then_with(|| {
            right
                .get("createdAt")
                .and_then(Value::as_str)
                .cmp(&left.get("createdAt").and_then(Value::as_str))
        })
    });

    Ok(Json(json!({ "items": items })))
}

pub(super) fn severity_rank(value: &str) -> i32 {
    match value.to_ascii_lowercase().as_str() {
        "error" | "critical" | "high" => 0,
        "warning" | "medium" => 1,
        _ => 2,
    }
}

/// Legge le ultime N righe di log da un servizio systemd --user via journalctl.
///
/// Ritorna UN SOLO evento contenente l'intero output, in ordine cronologico
/// (le righe piu' recenti in fondo, come `journalctl -f` o `tail`). In passato
/// l'output veniva chunkato in eventi da 50 righe, ognuno con header tipo
/// "righe 1951-2000": confondente per l'utente, che vedeva una pila di blocchi
/// con timestamp simili e numerazione contraria al concetto di tail.
///
/// Importante: usa `--since` legato a `ActiveEnterTimestamp` del servizio, così che
/// dopo ogni `restart` la finestra log si "resetti" automaticamente — l'utente vede
/// solo gli eventi del nuovo ciclo di vita del servizio, non l'intera storia che
/// includeva crash precedenti gia' risolti.
pub(super) async fn read_service_logs(service: &str, limit: usize, channel: &str) -> Vec<serde_json::Value> {
    // Tetto di righe restituite. `limit` arriva dal client (default 100, max 500
    // dopo clamp in get_output_events) — moltiplicato x10 per dare contesto
    // sufficiente, capped a 2000 per evitare payload enormi.
    let n_lines = (limit * 10).clamp(200, 2000).to_string();

    // Recupera il timestamp dell'ultimo avvio del servizio per il filtro --since.
    // ActiveEnterTimestamp e' in formato systemd ("Sun 2026-04-26 16:30:00 CEST"); journalctl
    // lo accetta direttamente come argomento di --since.
    let since: Option<String> = {
        let show = tokio::process::Command::new("systemctl")
            .args(["--user", "show", service, "--property=ActiveEnterTimestamp", "--no-pager"])
            .output()
            .await
            .ok();
        show.and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).to_string();
            s.lines()
                .find_map(|l| l.strip_prefix("ActiveEnterTimestamp=").map(|v| v.trim().to_string()))
                .filter(|v| !v.is_empty() && v != "n/a")
        })
    };

    let mut args: Vec<String> = vec![
        "--user".into(), "-u".into(), service.into(),
        "--no-pager".into(), "-n".into(), n_lines.clone(),
        "--output=short-iso".into(),
    ];
    if let Some(ref ts) = since {
        args.push("--since".into());
        args.push(ts.clone());
    }
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = tokio::process::Command::new("journalctl")
        .args(&args_ref)
        .output()
        .await;

    let text = match output {
        Ok(out) if out.status.success() || !out.stdout.is_empty() => {
            String::from_utf8_lossy(&out.stdout).to_string()
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            return vec![serde_json::json!({
                "id": format!("{}-err", service),
                "channel": channel,
                "level": "warn",
                "title": format!("Nessun log disponibile per {}", service),
                "text": if stderr.is_empty() { "journalctl non ha restituito output.".to_string() } else { stderr },
                "createdAt": chrono::Utc::now().to_rfc3339(),
            })];
        }
        Err(e) => {
            return vec![serde_json::json!({
                "id": format!("{}-err", service),
                "channel": channel,
                "level": "error",
                "title": format!("Errore lettura log {}", service),
                "text": e.to_string(),
                "createdAt": chrono::Utc::now().to_rfc3339(),
            })];
        }
    };

    if text.trim().is_empty() {
        let header = match since {
            Some(ref ts) => format!("Il servizio e' attivo dal {} ma non ha prodotto output dal restart.", ts),
            None => "Il servizio non ha prodotto output recente.".to_string(),
        };
        return vec![serde_json::json!({
            "id": format!("{}-empty", service),
            "channel": channel,
            "level": "info",
            "title": format!("{} — nessun log dal restart", service),
            "text": header,
            "createdAt": chrono::Utc::now().to_rfc3339(),
        })];
    }

    // Determina livello dal contenuto totale per evidenziare la pillola del canale
    let lower = text.to_lowercase();
    let level = if lower.contains(" error ") || lower.contains("error:") || lower.contains("panicked")
        || lower.contains("exception:") || lower.contains("[error]") {
        "error"
    } else if lower.contains(" warn ") || lower.contains("warning:") || lower.contains("[warn]") {
        "warn"
    } else {
        "info"
    };

    // Conta le righe per il titolo informativo
    let line_count = text.lines().count();
    let title = match since {
        Some(ts) => format!(
            "{} — ultimi {} log dal restart ({})",
            service, line_count, ts
        ),
        None => format!("{} — ultimi {} log", service, line_count),
    };

    // Un solo evento con tutto il flusso. Le righe sono in ordine cronologico
    // ascendente (piu' vecchie in cima, piu' recenti in fondo) come da default
    // di journalctl, cosi' l'auto-scroll del pannello si comporta come tail -f.
    vec![serde_json::json!({
        "id": format!("{}-tail", service),
        "channel": channel,
        "level": level,
        "title": title,
        "text": text,
        "createdAt": chrono::Utc::now().to_rfc3339(),
    })]
}

pub async fn get_output_channels(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;

    // Canali fissi di sistema
    let mut channels = vec![
        json!({ "id": "System",       "label": "System" }),
        json!({ "id": "Git",          "label": "Git" }),
        json!({ "id": "Tasks",        "label": "Tasks" }),
        json!({ "id": "Project Jobs", "label": "Project Jobs" }),
        json!({ "id": "Playwright",   "label": "Playwright" }),
        json!({ "id": "MCP Core",     "label": "MCP Core" }),
        json!({ "id": "Neural Core",  "label": "Neural Core" }),
    ];

    // Canali dinamici: uno per ogni servizio systemd del progetto ({slug}-*.service)
    let slug = context.details.name.to_lowercase().replace([' ', '_'], "-");
    let prefix = format!("{}-", slug);
    if let Ok(svc_out) = tokio::process::Command::new("systemctl")
        .args(["--user", "list-unit-files", "--type=service", "--no-legend", "--no-pager"])
        .output()
        .await
    {
        for line in String::from_utf8_lossy(&svc_out.stdout).lines() {
            let cols: Vec<&str> = line.split_whitespace().collect();
            let unit = cols.first().copied().unwrap_or("");
            let state = cols.get(1).copied().unwrap_or("");
            if unit.starts_with(&prefix) && unit.ends_with(".service") && state != "disabled" {
                let short = unit
                    .strip_prefix(&prefix).unwrap_or(unit)
                    .strip_suffix(".service").unwrap_or(unit);
                channels.push(json!({
                    "id":    format!("svc:{}", unit),
                    "label": short,
                    "title": unit,
                }));
            }
        }
    }

    // Canali dinamici agent: usati dal pannello Servizi (tab separato).
    // Self-healing in Rust: marca come 'stopped' nel DB i processi con status='running'
    // ma PID inesistente (residui di chat AI precedenti, restart Nexus, kill esterni).
    let agent_rows_raw = sqlx::query(
        "SELECT id, label, command, status, pid, COALESCE(kind, 'service') as kind FROM agent_processes \
         WHERE project_id = $1 \
         ORDER BY created_at DESC LIMIT 20",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    // Identifica fantasmi e li sana nel DB
    let mut orphan_ids: Vec<Uuid> = Vec::new();
    for row in &agent_rows_raw {
        let status: String = row.try_get::<String, _>("status").unwrap_or_default();
        if status != "running" { continue; }
        let pid: Option<i32> = row.try_get::<Option<i32>, _>("pid").ok().flatten();
        let alive = match pid {
            Some(p) if p > 0 => std::path::Path::new(&format!("/proc/{}", p)).exists(),
            _ => false,
        };
        if !alive {
            if let Ok(id) = row.try_get::<Uuid, _>("id") {
                orphan_ids.push(id);
            }
        }
    }
    if !orphan_ids.is_empty() {
        let _ = sqlx::query("UPDATE agent_processes SET status = 'stopped' WHERE id = ANY($1)")
            .bind(&orphan_ids)
            .execute(&state.db)
            .await;
    }

    // Filtra immediatamente i fantasmi dal risultato corrente, così la response è già pulita
    let orphan_set: std::collections::HashSet<Uuid> = orphan_ids.into_iter().collect();
    let agent_rows: Vec<_> = agent_rows_raw.into_iter().filter(|row| {
        let id: Uuid = row.try_get("id").unwrap_or_default();
        !orphan_set.contains(&id)
    }).collect();

    for row in &agent_rows {
        let proc_id: Uuid = row.get("id");
        let label: String = row.get("label");
        let status: String = row.get("status");
        let kind: String = row.get("kind");
        let display = if label.is_empty() {
            let cmd: String = row.get("command");
            cmd.chars().take(30).collect::<String>()
        } else {
            label
        };
        let icon = match status.as_str() {
            "running" => "● ",
            "failed" => "✗ ",
            "stopped" => "○ ",
            _ => "◌ ",
        };
        channels.push(json!({
            "id": format!("agent:{}", proc_id),
            "label": format!("{}{}", icon, display),
            "kind": kind
        }));
    }

    Ok(Json(json!({ "channels": channels })))
}

pub async fn get_output_events(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<BTreeMap<String, String>>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let channel = query
        .get("channel")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "System".to_string());
    let limit = query
        .get("limit")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(100)
        .clamp(1, 500);

    let events = match channel.as_str() {
        "Git" => {
            sqlx::query(
                r#"
                SELECT id, operation, status, stdout, stderr, created_at
                FROM git_operations
                WHERE project_id = $1
                ORDER BY created_at DESC
                LIMIT $2
                "#,
            )
            .bind(project_id)
            .bind(limit)
            .fetch_all(&state.db)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .into_iter()
            .map(|row| {
                json!({
                    "id": row.get::<Uuid, _>("id").to_string(),
                    "channel": "Git",
                    "level": if row.get::<String, _>("status") == "success" { "info" } else { "error" },
                    "title": row.get::<String, _>("operation"),
                    "text": format!(
                        "{}{}{}",
                        row.get::<String, _>("stdout"),
                        if !row.get::<String, _>("stdout").is_empty() && !row.get::<String, _>("stderr").is_empty() { "\n" } else { "" },
                        row.get::<String, _>("stderr")
                    ),
                    "createdAt": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
                })
            })
            .collect::<Vec<_>>()
        }
        "Tasks" | "Project Jobs" | "Playwright" => {
            let rows = sqlx::query(
                r#"
                SELECT id, kind, status, input, created_at
                FROM jobs
                WHERE project_id = $1
                  AND (
                    $2 = 'Project Jobs'
                    OR ($2 = 'Tasks' AND kind NOT ILIKE '%playwright%')
                    OR ($2 = 'Playwright' AND kind ILIKE '%playwright%')
                  )
                ORDER BY created_at DESC
                LIMIT $3
                "#,
            )
            .bind(project_id)
            .bind(channel.as_str())
            .bind(limit)
            .fetch_all(&state.db)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            rows.into_iter()
                .map(|row| {
                    let input = row.get::<Value, _>("input");
                    json!({
                        "id": row.get::<Uuid, _>("id").to_string(),
                        "channel": channel,
                        "level": if matches!(row.get::<String, _>("status").as_str(), "failed" | "error" | "cancelled") { "error" } else { "info" },
                        "title": row.get::<String, _>("kind"),
                        "text": input.get("message").and_then(Value::as_str).unwrap_or("Nessun output testuale disponibile"),
                        "createdAt": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
                    })
                })
                .collect::<Vec<_>>()
        }
        ch if ch.starts_with("agent:") => {
            // Singolo processo agent
            let proc_id_str = &ch[6..];
            let proc_id = Uuid::parse_str(proc_id_str)
                .map_err(|_| api_error(StatusCode::BAD_REQUEST, "process id non valido"))?;
            let row = sqlx::query(
                "SELECT id, label, command, status, exit_code, output, error_output, pid, created_at \
                 FROM agent_processes WHERE id = $1",
            )
            .bind(proc_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            match row {
                Some(row) => {
                    let status: String = row.get("status");
                    let output: String = row.get("output");
                    let error_output: String = row.get("error_output");
                    let label: String = row.get("label");
                    let command: String = row.get("command");
                    let pid: Option<i32> = row.try_get("pid").unwrap_or(None);
                    let exit_code: Option<i32> = row.try_get("exit_code").unwrap_or(None);
                    let title = format!(
                        "{} [pid: {}, status: {}{}]",
                        if label.is_empty() { &command } else { &label },
                        pid.map(|p| p.to_string()).unwrap_or_else(|| "?".into()),
                        status,
                        exit_code.map(|c| format!(", exit: {}", c)).unwrap_or_default(),
                    );
                    let text = if error_output.is_empty() {
                        output
                    } else {
                        format!("{}\n--- STDERR ---\n{}", output, error_output)
                    };
                    vec![json!({
                        "id": proc_id.to_string(),
                        "channel": channel,
                        "level": if status == "failed" { "error" } else { "info" },
                        "title": title,
                        "text": text,
                        "createdAt": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
                    })]
                }
                None => vec![],
            }
        }
        "System" => {
            // Stato di TUTTI i servizi del progetto rilevati dinamicamente
            let slug = context.details.name.to_lowercase().replace([' ', '_'], "-");
            let prefix = format!("{}-", slug);
            let mut lines = Vec::new();

            if let Ok(list_out) = tokio::process::Command::new("systemctl")
                .args(["--user", "list-units", "--type=service", "--all", "--no-legend", "--no-pager"])
                .output()
                .await
            {
                let list_str = String::from_utf8_lossy(&list_out.stdout);
                for line in list_str.lines() {
                    let cols: Vec<&str> = line.split_whitespace().collect();
                    if cols.len() < 4 { continue; }
                    let unit = cols[0].trim_start_matches('●').trim();
                    if !unit.starts_with(&prefix) || !unit.ends_with(".service") { continue; }
                    let active = cols[2];
                    let sub    = cols[3];
                    lines.push(format!("{}: {} ({})", unit, active, sub));
                }
            }

            if lines.is_empty() {
                lines.push(format!("Nessun servizio trovato con prefisso '{}'.", prefix));
            }

            lines.push(String::new());
            lines.push(format!("Progetto: {}", context.details.name));
            lines.push(format!("Root: {}", context.root_path.to_string_lossy()));

            vec![json!({
                "id": format!("system-{}", context.project_id),
                "channel": "System",
                "level": "info",
                "title": "System status",
                "text": lines.join("\n"),
                "createdAt": chrono::Utc::now().to_rfc3339(),
            })]
        }
        ch if ch.starts_with("svc:") => {
            // Canale dinamico servizio: "svc:{unit_name}"
            let unit = ch.strip_prefix("svc:").unwrap_or(ch);
            let slug = context.details.name.to_lowercase().replace([' ', '_'], "-");
            let prefix = format!("{}-", slug);
            // Verifica di appartenenza al progetto
            if !unit.starts_with(&prefix) {
                return Ok(Json(json!({ "channel": channel, "events": [] })));
            }
            read_service_logs(unit, limit as usize, &channel).await
        }
        _ => {
            vec![json!({
                "id": format!("system-{}", context.project_id),
                "channel": channel,
                "level": "info",
                "title": "Project context",
                "text": format!(
                    "Progetto attivo: {}\nRoot: {}\nRepository: {}",
                    context.details.name,
                    context.root_path.to_string_lossy(),
                    context.repository_root_path.to_string_lossy()
                ),
                "createdAt": chrono::Utc::now().to_rfc3339(),
            })]
        }
    };

    Ok(Json(json!({ "channel": channel, "events": events })))
}

pub async fn get_playwright_runs(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    let rows = sqlx::query(
        r#"
        SELECT id, kind, status, input, created_at, updated_at, progress
        FROM jobs
        WHERE project_id = $1 AND kind ILIKE '%playwright%'
        ORDER BY created_at DESC
        LIMIT 50
        "#,
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let runs = rows
        .into_iter()
        .map(|row| {
            let input = row.get::<Value, _>("input");
            let progress = row.try_get::<Value, _>("progress").unwrap_or_else(|_| json!({}));
            json!({
                "id": row.get::<Uuid, _>("id").to_string(),
                "label": input.get("label").and_then(Value::as_str).unwrap_or("Playwright run"),
                "status": row.get::<String, _>("status"),
                "summary": input.get("message").and_then(Value::as_str),
                "artifacts": input.get("artifacts").cloned().unwrap_or_else(|| json!([])),
                "command": input.get("command").and_then(Value::as_str),
                "exitCode": input.get("exit_code").and_then(Value::as_i64),
                "progress": progress,
                "createdAt": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
                "updatedAt": row.try_get::<chrono::DateTime<chrono::Utc>, _>("updated_at")
                    .map(|d| d.to_rfc3339())
                    .unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();

    // Verifica se Playwright e' configurato (config file nella project root)
    let project_root: Option<String> = sqlx::query_scalar(
        "SELECT project_root FROM projects WHERE id = $1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    let configured = if let Some(root) = &project_root {
        let root_path = std::path::Path::new(root);
        root_path.join("playwright.config.ts").exists()
            || root_path.join("playwright.config.js").exists()
            || root_path.join("playwright.config.mjs").exists()
    } else {
        false
    };

    Ok(Json(json!({ "runs": runs, "configured": configured })))
}

/// GET /api/projects/:id/playwright/runs/:run_id  — dettaglio singolo run con output_log.
pub async fn get_playwright_run_detail(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((project_id_str, run_id_str)): AxumPath<(String, String)>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let run_id = Uuid::parse_str(&run_id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Run id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    let row = sqlx::query(
        r#"
        SELECT id, status, input, created_at, updated_at, progress, output_log
        FROM jobs
        WHERE id = $1 AND project_id = $2 AND kind = 'playwright_test'
        "#,
    )
    .bind(run_id)
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Run non trovato"))?;

    let input = row.get::<Value, _>("input");
    let progress = row.try_get::<Value, _>("progress").unwrap_or_else(|_| json!({}));
    let output_log = row
        .try_get::<Option<String>, _>("output_log")
        .ok()
        .flatten()
        .unwrap_or_default();

    Ok(Json(json!({
        "id": row.get::<Uuid, _>("id").to_string(),
        "status": row.get::<String, _>("status"),
        "label": input.get("label").and_then(Value::as_str).unwrap_or("Playwright run"),
        "command": input.get("command").and_then(Value::as_str),
        "summary": input.get("message").and_then(Value::as_str),
        "artifacts": input.get("artifacts").cloned().unwrap_or_else(|| json!([])),
        "exitCode": input.get("exit_code").and_then(Value::as_i64),
        "progress": progress,
        "outputLog": output_log,
        "createdAt": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
        "updatedAt": row.try_get::<chrono::DateTime<chrono::Utc>, _>("updated_at")
            .map(|d| d.to_rfc3339())
            .unwrap_or_default(),
    })))
}

/// GET /api/projects/:id/playwright/runs/:run_id/stream  — SSE stream eventi live.
///
/// Eventi emessi (SSE event types):
/// - `line`: una riga di output Playwright
/// - `progress`: counter aggiornati (passed/failed/skipped/current_spec)
/// - `final`: status terminale (passed/failed/timeout)
///
/// Quando il run e' gia' terminato (no channel attivo), ritorna subito un
/// evento `final` con lo stato dal DB e chiude.
pub async fn stream_playwright_run(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((project_id_str, run_id_str)): AxumPath<(String, String)>,
) -> Result<axum::response::Sse<std::pin::Pin<Box<dyn futures::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>> + Send>>>, (StatusCode, Json<Value>)> {
    use axum::response::sse::{Event, Sse};
    use futures::StreamExt;

    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let run_id = Uuid::parse_str(&run_id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Run id non valido"))?;
    let _ = load_project_context(&state.db, project_id, user_id).await?;

    // Verifica che il job appartenga al progetto
    let job_exists: Option<String> = sqlx::query_scalar(
        "SELECT status FROM jobs WHERE id = $1 AND project_id = $2 AND kind = 'playwright_test'"
    )
    .bind(run_id)
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if job_exists.is_none() {
        return Err(api_error(StatusCode::NOT_FOUND, "Run non trovato"));
    }

    // Si aggancia al channel se attivo
    let rx_opt = state.playwright_channels.get(&run_id).map(|tx| tx.subscribe());

    let stream: std::pin::Pin<Box<dyn futures::Stream<Item = Result<Event, std::convert::Infallible>> + Send>> = match rx_opt {
        Some(rx) => {
            // Run live: stream gli eventi finche' il sender e' aperto
            Box::pin(
                tokio_stream::wrappers::BroadcastStream::new(rx)
                    .map(|res| match res {
                        Ok(ev) => {
                            let event_type = match &ev {
                                crate::playwright_live::PlaywrightEvent::Line { .. } => "line",
                                crate::playwright_live::PlaywrightEvent::Progress { .. } => "progress",
                                crate::playwright_live::PlaywrightEvent::Final { .. } => "final",
                            };
                            let data = serde_json::to_string(&ev).unwrap_or_default();
                            Ok::<_, std::convert::Infallible>(
                                Event::default().event(event_type).data(data)
                            )
                        }
                        Err(_) => Ok(Event::default().event("error").data("lag")),
                    })
            )
        }
        None => {
            // Run gia' chiuso: emette singolo evento "final" con stato DB e termina.
            // Costruisce sincrono lo state finale e lo wrappa in tokio_stream::once.
            let row_opt = sqlx::query(
                "SELECT status, progress, input FROM jobs WHERE id = $1"
            )
            .bind(run_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
            let payload = if let Some(row) = row_opt {
                let status: String = row.try_get("status").unwrap_or_else(|_| "unknown".into());
                let progress: Value = row.try_get("progress").unwrap_or_else(|_| json!({}));
                let input: Value = row.try_get("input").unwrap_or_else(|_| json!({}));
                let exit_code = input.get("exit_code").and_then(Value::as_i64).unwrap_or(-1);
                json!({
                    "kind": "final",
                    "job_id": run_id.to_string(),
                    "status": status,
                    "exit_code": exit_code,
                    "progress": progress,
                })
            } else {
                json!({ "kind": "final", "job_id": run_id.to_string(), "status": "unknown" })
            };
            let ev = Event::default().event("final").data(payload.to_string());
            Box::pin(tokio_stream::once(Ok::<_, std::convert::Infallible>(ev)))
        }
    };

    Ok(Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keep-alive")
    ))
}

#[derive(serde::Deserialize)]
pub struct ArtifactQuery {
    pub path: String,
}

pub async fn serve_playwright_artifact(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    axum::extract::Query(q): axum::extract::Query<ArtifactQuery>,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    use axum::http::header;
    use axum::response::IntoResponse;

    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;

    // Path traversal guard: il path richiesto deve essere relativo e risolversi
    // dentro la project root; deve inoltre puntare a test-results/ o playwright-report/.
    let rel = std::path::Path::new(&q.path);
    if rel.is_absolute() || q.path.contains("..") {
        return Err(api_error(StatusCode::BAD_REQUEST, "path non valido"));
    }
    let allowed = q.path.contains("test-results/")
        || q.path.contains("playwright-report/")
        || q.path.contains("test-results\\")
        || q.path.contains("playwright-report\\");
    if !allowed {
        return Err(api_error(StatusCode::FORBIDDEN, "solo artifact Playwright sono accessibili"));
    }
    let full = context.root_path.join(rel);
    let canonical = full
        .canonicalize()
        .map_err(|_| api_error(StatusCode::NOT_FOUND, "file non trovato"))?;
    let root_canonical = context
        .root_path
        .canonicalize()
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "project root non risolvibile"))?;
    if !canonical.starts_with(&root_canonical) {
        return Err(api_error(StatusCode::FORBIDDEN, "path fuori dal progetto"));
    }

    let bytes = tokio::fs::read(&canonical)
        .await
        .map_err(|e| api_error(StatusCode::NOT_FOUND, format!("lettura file: {}", e)))?;

    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "webm" => "video/webm",
        "mp4" => "video/mp4",
        "zip" => "application/zip",
        "html" => "text/html; charset=utf-8",
        _ => "application/octet-stream",
    };

    Ok(([(header::CONTENT_TYPE, mime)], bytes).into_response())
}

pub async fn clear_playwright_runs(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    let result = sqlx::query(
        r#"DELETE FROM jobs WHERE project_id = $1 AND kind ILIKE '%playwright%'"#,
    )
    .bind(project_id)
    .execute(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let deleted = result.rows_affected();
    // Dispatcher: notifica al frontend di svuotare il pannello Playwright in tempo reale
    nexus_events::dispatcher::emit(
        &state.project_channels,
        project_id,
        nexus_events::event::ProjectEvent::JobsCleared {
            job_kind: "playwright_test".to_string(),
            deleted,
        },
    );

    Ok(Json(json!({ "deleted": deleted })))
}
