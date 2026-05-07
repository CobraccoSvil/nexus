use super::*;

// ── POST /api/projects/:id/services/:service/:action ─────────────────────────
// service: "backend" | "brain" | "frontend"
// action:  "start" | "stop" | "restart"

/// Elenca tutti i servizi systemd --user il cui nome inizia con `{slug}-`.
/// Nessun hardcoding: il progetto può avere quanti servizi vuole.
pub async fn get_project_services_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let slug = context.details.name.to_lowercase().replace([' ', '_'], "-");

    // `systemctl --user list-units --type=service --all --no-legend --no-pager`
    // restituisce righe: "  UNIT  LOAD  ACTIVE  SUB  DESCRIPTION"
    let out = tokio::process::Command::new("systemctl")
        .args(["--user", "list-units", "--type=service", "--all", "--no-legend", "--no-pager"])
        .output()
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let prefix = format!("{}-", slug);
    let stdout = String::from_utf8_lossy(&out.stdout);

    let mut services: Vec<serde_json::Value> = Vec::new();
    for line in stdout.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 { continue; }
        let unit = cols[0].trim_start_matches('●').trim();
        if !unit.starts_with(&prefix) || !unit.ends_with(".service") { continue; }
        let active = cols[2]; // "active" | "inactive" | "failed" | ...
        let sub    = cols[3]; // "running" | "exited" | "dead" | ...
        // nome corto: rimuove il prefisso slug e il suffisso .service
        let short = unit
            .strip_prefix(&prefix).unwrap_or(unit)
            .strip_suffix(".service").unwrap_or(unit);

        let mut entry = json!({
            "unit":   unit,
            "short":  short,
            "state":  active,
            "sub":    sub,
        });

        // Se il servizio e' in crash-loop o failed, leggi il journal per diagnosticare.
        // Rileva anche servizi momentaneamente "active" ma con NRestarts elevato
        // (es. dotnet run che impiega 40s per la build prima di fallire).
        let is_failing = (active == "activating" && sub == "auto-restart")
            || active == "failed";
        let is_crash_looping = if !is_failing && active == "active" {
            // Controlla NRestarts: se > 2, il servizio sta ciclando
            tokio::process::Command::new("systemctl")
                .args(["--user", "show", unit, "--property=NRestarts"])
                .output()
                .await
                .ok()
                .and_then(|o| {
                    let s = String::from_utf8_lossy(&o.stdout).to_string();
                    s.trim().strip_prefix("NRestarts=")
                        .and_then(|v| v.parse::<u32>().ok())
                })
                .map_or(false, |n| n > 2)
        } else {
            false
        };

        if is_failing || is_crash_looping {
            if let Ok(journal) = tokio::process::Command::new("journalctl")
                .args(["--user", "-u", unit, "--no-pager", "-n", "20", "-o", "cat"])
                .output()
                .await
            {
                let log = String::from_utf8_lossy(&journal.stdout).to_string();
                let diag = diagnose_service_failure(&log, unit, &context.root_path);
                entry["last_error"] = json!(diag.error);
                entry["suggestion"] = json!(diag.suggestion);
                entry["error_kind"] = json!(diag.kind);
                if is_crash_looping && !is_failing {
                    entry["crash_loop"] = json!(true);
                }
            }
        }

        services.push(entry);
    }

    Ok(Json(json!({ "services": services, "slug": slug })))
}

/// Controlla un servizio del progetto: start | stop | restart.
/// Il parametro `service` è il nome corto (senza prefisso slug e senza .service),
/// ad es. "backend", "api-v2", "worker-email".
/// Il backend verifica che il nome dell'unità risultante inizi con `{slug}-`.
pub async fn control_project_service(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path((id, service, action)): axum::extract::Path<(String, String, String)>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;

    let slug = context.details.name.to_lowercase().replace([' ', '_'], "-");

    // Sicurezza: il service name non può contenere '/' o '..' e deve iniziare col prefisso slug
    if service.contains('/') || service.contains("..") {
        return Err(api_error(StatusCode::BAD_REQUEST, "Nome servizio non valido"));
    }

    // Costruisce il nome unit: se il chiamante manda già il nome completo (slug-xxx) lo usa,
    // altrimenti lo antepone.
    let svc_name = if service.starts_with(&format!("{}-", slug)) {
        format!("{}.service", service)
    } else {
        format!("{}-{}.service", slug, service)
    };

    let systemctl_action = match action.as_str() {
        "start" | "stop" | "restart" => action.as_str(),
        other => return Err(api_error(StatusCode::BAD_REQUEST, format!("Azione non valida: {}", other))),
    };

    // Pre-check: prima di start/restart, libera le porte occupate da processi estranei
    let mut freed_ports: Vec<serde_json::Value> = Vec::new();
    if systemctl_action == "start" || systemctl_action == "restart" {
        freed_ports = free_ports_for_unit(&svc_name).await;
        if !freed_ports.is_empty() {
            tracing::info!("Pre-start {}: liberate {} porte occupate", svc_name, freed_ports.len());
        }
    }

    let out = tokio::process::Command::new("systemctl")
        .args(["--user", systemctl_action, &svc_name])
        .output()
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let ok = out.status.success();

    Ok(Json(json!({
        "ok":     ok,
        "unit":   svc_name,
        "action": systemctl_action,
        "stdout": stdout,
        "stderr": stderr,
        "freed_ports": freed_ports,
    })))
}

// ── POST /api/projects/:id/services/restart-all ─────────────────────────────
/// Riavvia in batch tutti i `{slug}-*.service` del progetto.
pub async fn restart_all_project_services(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let slug = context.details.name.to_lowercase().replace([' ', '_'], "-");

    // Lista delle unit del progetto
    let prefix = format!("{}-", slug);
    let list = tokio::process::Command::new("systemctl")
        .args(["--user", "list-units", "--type=service", "--all", "--no-legend", "--no-pager"])
        .output()
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let list_str = String::from_utf8_lossy(&list.stdout);
    let units: Vec<String> = list_str
        .lines()
        .filter_map(|line| {
            let unit = line.split_whitespace().next()?;
            if unit.starts_with(&prefix) { Some(unit.to_string()) } else { None }
        })
        .collect();

    let mut results = Vec::new();
    for unit in &units {
        let freed = free_ports_for_unit(unit).await;
        let out = tokio::process::Command::new("systemctl")
            .args(["--user", "restart", unit])
            .output()
            .await;
        match out {
            Ok(o) => results.push(json!({
                "unit": unit,
                "ok": o.status.success(),
                "stderr": String::from_utf8_lossy(&o.stderr).to_string(),
                "freed_ports": freed,
            })),
            Err(e) => results.push(json!({ "unit": unit, "ok": false, "stderr": e.to_string() })),
        }
    }
    Ok(Json(json!({ "slug": slug, "restarted": results })))
}

/// POST /api/projects/:id/services/stop-all
/// Ferma tutti i `{slug}-*.service` del progetto e rilascia le porte dal port_registry.
pub async fn stop_all_project_services(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let slug = context.details.name.to_lowercase().replace([' ', '_'], "-");

    let prefix = format!("{}-", slug);
    let list = tokio::process::Command::new("systemctl")
        .args(["--user", "list-units", "--type=service", "--all", "--no-legend", "--no-pager"])
        .output()
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let list_str = String::from_utf8_lossy(&list.stdout);
    let units: Vec<String> = list_str
        .lines()
        .filter_map(|line| {
            let unit = line.split_whitespace().next()?;
            if unit.starts_with(&prefix) { Some(unit.to_string()) } else { None }
        })
        .collect();

    // Helper: rilascia porte dal registry leggendo il unit file.
    async fn release_ports_for_unit(state: &AppState, unit_name: &str) {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let unit_path = format!("{}/.config/systemd/user/{}", home, unit_name);
        if let Ok(content) = tokio::fs::read_to_string(&unit_path).await {
            let ports = extract_ports_from_unit(&content);
            for p in ports {
                let _ = state.port_registry.release(p).await;
            }
        }
    }

    let mut stopped = Vec::new();
    for unit in &units {
        let out = tokio::process::Command::new("systemctl")
            .args(["--user", "stop", unit])
            .output()
            .await;
        release_ports_for_unit(&state, unit).await;
        match out {
            Ok(o) => stopped.push(json!({
                "unit": unit,
                "ok": o.status.success(),
                "stderr": String::from_utf8_lossy(&o.stderr).to_string(),
            })),
            Err(e) => stopped.push(json!({ "unit": unit, "ok": false, "stderr": e.to_string() })),
        }
    }
    Ok(Json(json!({ "slug": slug, "stopped": stopped })))
}

// ── POST /api/projects/:id/services/cleanup-ports ───────────────────────────
/// Termina i processi che occupano porte rilevate per il progetto MA non sono
/// gestiti da systemd `{slug}-*.service` (porte "orfane" o conflittuali).
/// Body opzionale: { "ports": [3002, 5215, ...] } per limitare l'azione.
pub async fn cleanup_project_ports(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    body: Option<axum::Json<serde_json::Value>>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let slug = context.details.name.to_lowercase().replace([' ', '_'], "-");

    // Porte target: dal body o, se assente, tutte quelle rilevate nel progetto
    let target_ports: std::collections::HashSet<u16> = match body {
        Some(axum::Json(b)) => b.get("ports")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_u64()).map(|n| n as u16).collect())
            .unwrap_or_default(),
        None => std::collections::HashSet::new(),
    };

    // Raccoglie i MainPID dei servizi systemd del progetto (PID protetti)
    let prefix = format!("{}-", slug);
    let list_out = tokio::process::Command::new("systemctl")
        .args(["--user", "list-units", "--type=service", "--all", "--no-legend", "--no-pager"])
        .output()
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let list_str = String::from_utf8_lossy(&list_out.stdout);
    let units: Vec<String> = list_str
        .lines()
        .filter_map(|line| {
            let unit = line.split_whitespace().next()?;
            if unit.starts_with(&prefix) { Some(unit.to_string()) } else { None }
        })
        .collect();

    let mut protected_pids: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for unit in &units {
        let show_out = tokio::process::Command::new("systemctl")
            .args(["--user", "show", unit, "--property=MainPID"])
            .output()
            .await
            .ok();
        if let Some(o) = show_out {
            let s = String::from_utf8_lossy(&o.stdout);
            for line in s.lines() {
                if let Some(val) = line.strip_prefix("MainPID=") {
                    if let Ok(pid) = val.trim().parse::<u32>() {
                        if pid > 0 { protected_pids.insert(pid); }
                    }
                }
            }
        }
    }

    // Espande PID protetti con tutti i discendenti (BFS) per non ucciderli per sbaglio
    let mut children: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
    if let Ok(proc_entries) = std::fs::read_dir("/proc") {
        for entry in proc_entries.flatten() {
            let n = entry.file_name();
            let s = n.to_string_lossy();
            if let Ok(pid) = s.parse::<u32>() {
                if let Ok(content) = std::fs::read_to_string(format!("/proc/{}/status", pid)) {
                    for line in content.lines() {
                        if let Some(rest) = line.strip_prefix("PPid:") {
                            if let Ok(ppid) = rest.trim().parse::<u32>() {
                                children.entry(ppid).or_default().push(pid);
                            }
                            break;
                        }
                    }
                }
            }
        }
    }
    let mut queue: std::collections::VecDeque<u32> = protected_pids.iter().copied().collect();
    while let Some(pid) = queue.pop_front() {
        if let Some(kids) = children.get(&pid) {
            for &c in kids {
                if protected_pids.insert(c) { queue.push_back(c); }
            }
        }
    }

    // Trova tutti i processi che ascoltano sulle porte e killa quelli non protetti
    let listening = read_listening_ports_ss().await
        .unwrap_or_else(|_| read_listening_ports_proc());

    let mut killed = Vec::new();
    let mut skipped = Vec::new();
    for (port, pid, program) in listening {
        // Se è stata data una whitelist di porte, applica il filtro
        if !target_ports.is_empty() && !target_ports.contains(&port) {
            continue;
        }
        if protected_pids.contains(&pid) {
            skipped.push(json!({ "port": port, "pid": pid, "program": program, "reason": "protetto (servizio del progetto)" }));
            continue;
        }
        // Esegue kill -TERM, fallback -KILL dopo 1s
        let _ = tokio::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .output()
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        let still_alive = std::path::Path::new(&format!("/proc/{}", pid)).exists();
        if still_alive {
            let _ = tokio::process::Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .output()
                .await;
        }
        killed.push(json!({ "port": port, "pid": pid, "program": program }));
    }

    Ok(Json(json!({
        "slug": slug,
        "killed": killed,
        "skipped": skipped,
    })))
}

pub async fn get_project_ports(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let project_root = context.root_path.to_string_lossy().to_string();
    let slug = context.details.name.to_lowercase().replace([' ', '_'], "-");

    let ports = detect_project_ports(&project_root, &slug, project_id, &state.db).await;
    Ok(Json(json!({ "ports": ports })))
}

/// Rileva le porte TCP in ascolto associate ai processi del progetto.
/// Strategia:
/// 1. Legge i PID dei processi agent_processes in esecuzione per il progetto
/// 2. Aggiunge i MainPID dei servizi systemd --user con prefisso {slug}-
/// 3. Scansiona /proc per qualsiasi processo con cwd nel project_root
/// 4. Espande con tutti i processi discendenti
pub(super) async fn detect_project_ports(
    project_root: &str,
    slug: &str,
    project_id: Uuid,
    db: &sqlx::PgPool,
) -> Vec<serde_json::Value> {
    let mut ports: Vec<serde_json::Value> = Vec::new();

    // 1. PID dai processi agent — include sia 'running' che altri status purché il processo sia ancora vivo.
    // Lo status nel DB può essere 'failed' dopo un riavvio di mcp-core anche se il processo gira ancora.
    let agent_pids: Vec<i32> = sqlx::query(
        "SELECT pid FROM agent_processes WHERE project_id = $1 AND pid IS NOT NULL"
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .iter()
    .filter_map(|row| row.try_get::<i32, _>("pid").ok())
    // Verifica che il processo sia ancora vivo controllando /proc/{pid}
    .filter(|pid| std::path::Path::new(&format!("/proc/{}", pid)).exists())
    .collect();

    // 2a. MainPID dei servizi systemd --user `{slug}-*.service` + mappa pid→short_name
    let svc_prefix = format!("{}-", slug);
    let mut pid_to_service: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    let systemd_pids: Vec<u32> = {
        let list_out = tokio::process::Command::new("systemctl")
            .args(["--user", "list-units", "--type=service", "--all", "--no-legend", "--no-pager"])
            .output()
            .await
            .unwrap_or_else(|_| std::process::Output {
                status: std::process::ExitStatus::default(),
                stdout: vec![],
                stderr: vec![],
            });
        let list_str = String::from_utf8_lossy(&list_out.stdout);
        let units: Vec<String> = list_str
            .lines()
            .filter_map(|line| {
                let unit = line.split_whitespace().next()?;
                if unit.starts_with(&svc_prefix) { Some(unit.to_string()) } else { None }
            })
            .collect();

        let mut pids = Vec::new();
        for unit in &units {
            let short = unit
                .strip_prefix(&svc_prefix).unwrap_or(unit)
                .strip_suffix(".service").unwrap_or(unit)
                .to_string();
            let show_out = tokio::process::Command::new("systemctl")
                .args(["--user", "show", unit, "--property=MainPID"])
                .output()
                .await
                .unwrap_or_else(|_| std::process::Output {
                    status: std::process::ExitStatus::default(),
                    stdout: vec![],
                    stderr: vec![],
                });
            let show_str = String::from_utf8_lossy(&show_out.stdout);
            for line in show_str.lines() {
                if let Some(val) = line.strip_prefix("MainPID=") {
                    if let Ok(pid) = val.trim().parse::<u32>() {
                        if pid > 0 && std::path::Path::new(&format!("/proc/{}", pid)).exists() {
                            pids.push(pid);
                            pid_to_service.insert(pid, short.clone());
                        }
                    }
                }
            }
        }
        pids
    };

    // 2b. Raccogli tutti i PID rilevanti: agent + systemd + processi con cwd = project_root
    let mut all_pids: std::collections::HashSet<u32> = agent_pids.iter().map(|p| *p as u32)
        .chain(systemd_pids.into_iter())
        .collect();

    // Costruisce mappa ppid → vec<pid> per trovare i processi figli (es. node figlio di pnpm)
    let mut children: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
    if let Ok(proc_entries) = std::fs::read_dir("/proc") {
        for entry in proc_entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if let Ok(pid) = name_str.parse::<u32>() {
                // Leggi ppid da /proc/{pid}/status
                let status_path = format!("/proc/{}/status", pid);
                if let Ok(content) = std::fs::read_to_string(&status_path) {
                    for line in content.lines() {
                        if let Some(rest) = line.strip_prefix("PPid:") {
                            if let Ok(ppid) = rest.trim().parse::<u32>() {
                                children.entry(ppid).or_default().push(pid);
                            }
                            break;
                        }
                    }
                }
                // Scansiona cwd per aggiungere qualsiasi processo con cwd nel project_root
                let cwd_path = format!("/proc/{}/cwd", pid);
                if let Ok(cwd) = std::fs::read_link(&cwd_path) {
                    let cwd_str = cwd.to_string_lossy();
                    if cwd_str.starts_with(project_root) {
                        all_pids.insert(pid);
                    }
                }
            }
        }
    }

    // Espandi all_pids con tutti i discendenti dei PID già noti (BFS).
    let mut queue: std::collections::VecDeque<u32> = all_pids.iter().copied().collect();
    while let Some(pid) = queue.pop_front() {
        if let Some(kids) = children.get(&pid) {
            for &child in kids {
                if all_pids.insert(child) {
                    queue.push_back(child);
                }
            }
        }
    }

    // Propagazione `pid_to_service` ai discendenti: BFS dedicata che parte SOLO dai MainPID
    // systemd (gli unici che hanno un service noto a priori) e scende l'albero processi
    // ignorando l'appartenenza a all_pids — così l'ordine delle passate non perde i match
    // anche se un figlio era già stato raccolto via cwd_match.
    let initial_svc_pids: Vec<u32> = pid_to_service.keys().copied().collect();
    let mut svc_queue: std::collections::VecDeque<u32> = initial_svc_pids.into_iter().collect();
    while let Some(pid) = svc_queue.pop_front() {
        let parent_svc = match pid_to_service.get(&pid).cloned() {
            Some(s) => s,
            None => continue,
        };
        if let Some(kids) = children.get(&pid) {
            for &child in kids {
                let was_new = !pid_to_service.contains_key(&child);
                pid_to_service.entry(child).or_insert_with(|| parent_svc.clone());
                if was_new {
                    svc_queue.push_back(child);
                }
            }
        }
    }

    if all_pids.is_empty() {
        return ports;
    }

    // 3. Leggi le porte TCP in ascolto tramite ss oppure /proc/net/tcp
    let listening = read_listening_ports_ss().await
        .unwrap_or_else(|_| read_listening_ports_proc());

    for (port_num, pid, program) in listening {
        if all_pids.contains(&pid) {
            let label = if program.is_empty() {
                format!("Porta {}", port_num)
            } else {
                program.clone()
            };
            let url = format!("http://localhost:{}", port_num);
            let service = pid_to_service.get(&pid).cloned();
            ports.push(json!({
                "port": port_num,
                "label": label,
                "pid": pid,
                "state": "LISTEN",
                "url": url,
                "service": service,
            }));
        }
    }

    // 4. Container Docker associati ai servizi del progetto: nome con prefisso slug
    if let Ok(docker_out) = tokio::process::Command::new("docker")
        .args(["ps", "--format", "{{.Names}}|{{.Ports}}"])
        .output()
        .await
    {
        let docker_str = String::from_utf8_lossy(&docker_out.stdout);
        let docker_prefix1 = format!("{}-", slug);
        let docker_prefix2 = format!("{}_", slug);
        for line in docker_str.lines() {
            let parts: Vec<&str> = line.splitn(2, '|').collect();
            if parts.len() != 2 { continue; }
            let cname = parts[0].trim();
            // Filtra container appartenenti al progetto (per nome o per project label di docker-compose)
            if !cname.starts_with(&docker_prefix1)
                && !cname.starts_with(&docker_prefix2)
                && !cname.contains(slug)
            {
                continue;
            }
            // Esempio porte: "0.0.0.0:5215->8080/tcp, [::]:5215->8080/tcp"
            for entry in parts[1].split(',') {
                let entry = entry.trim();
                // Estrae la porta host: cerca pattern host_port->container_port
                if let Some(arrow_pos) = entry.find("->") {
                    let host_part = &entry[..arrow_pos];
                    let host_port: u16 = host_part.rsplit(':').next()
                        .and_then(|p| p.parse().ok())
                        .unwrap_or(0);
                    if host_port > 0 {
                        // Tenta di derivare lo "short" del servizio dal nome container:
                        // redemptor-backend-dev → "backend"; redemptor-sqlserver-dev → "sqlserver"
                        let svc_guess = cname
                            .strip_prefix(&docker_prefix1).or_else(|| cname.strip_prefix(&docker_prefix2))
                            .map(|rest| {
                                rest.trim_end_matches("-dev")
                                    .trim_end_matches("-prod")
                                    .trim_end_matches("_dev")
                                    .trim_end_matches("_prod")
                                    .to_string()
                            });
                        ports.push(json!({
                            "port":    host_port,
                            "label":   format!("docker:{}", cname),
                            "pid":     0,
                            "state":   "LISTEN",
                            "url":     format!("http://localhost:{}", host_port),
                            "service": svc_guess,
                        }));
                    }
                }
            }
        }
    }

    // Dedup per porta
    ports.sort_by_key(|p| p["port"].as_u64().unwrap_or(0));
    ports.dedup_by_key(|p| p["port"].as_u64().unwrap_or(0));
    ports
}

/// Legge porte TCP in ascolto via `ss -tlnp` → Vec<(port, pid, program)>
pub(super) async fn read_listening_ports_ss() -> anyhow::Result<Vec<(u16, u32, String)>> {
    let output = tokio::process::Command::new("ss")
        .args(["-tlnp"])
        .output()
        .await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut result = Vec::new();
    for line in stdout.lines().skip(1) {
        // Esempio: LISTEN 0 128 0.0.0.0:3000 0.0.0.0:* users:(("node",pid=1234,fd=5))
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 5 { continue; }
        let local_addr = parts.get(3).unwrap_or(&"");
        let port: u16 = local_addr.rsplit(':').next()
            .and_then(|p| p.parse().ok())
            .unwrap_or(0);
        if port == 0 { continue; }
        // Estrai pid e program da users:(("program",pid=NNN,fd=N))
        let users_str = parts[4..].join(" ");
        let pid = users_str.split("pid=")
            .nth(1)
            .and_then(|s| s.split(',').next())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let program = users_str.split('"')
            .nth(1)
            .unwrap_or("")
            .to_string();
        if pid > 0 {
            result.push((port, pid, program));
        }
    }
    Ok(result)
}

/// Fallback: legge /proc/net/tcp e /proc/net/tcp6 → Vec<(port, pid, program)>
pub(super) fn read_listening_ports_proc() -> Vec<(u16, u32, String)> {
    let mut inode_to_port: std::collections::HashMap<u64, u16> = std::collections::HashMap::new();

    for path in &["/proc/net/tcp", "/proc/net/tcp6"] {
        if let Ok(content) = std::fs::read_to_string(path) {
            for line in content.lines().skip(1) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() < 10 { continue; }
                // stato 0A = LISTEN
                if parts[3] != "0A" { continue; }
                // local_address es. 00000000:0BB8
                let port = u16::from_str_radix(
                    parts[1].split(':').nth(1).unwrap_or("0"), 16
                ).unwrap_or(0);
                let inode: u64 = parts[9].parse().unwrap_or(0);
                if port > 0 && inode > 0 {
                    inode_to_port.insert(inode, port);
                }
            }
        }
    }

    // Mappa inode → pid via /proc/{pid}/fd/*
    let mut result = Vec::new();
    if let Ok(proc_entries) = std::fs::read_dir("/proc") {
        for entry in proc_entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let Ok(pid) = name_str.parse::<u32>() else { continue };
            let fd_dir = format!("/proc/{}/fd", pid);
            let Ok(fds) = std::fs::read_dir(&fd_dir) else { continue };
            for fd in fds.flatten() {
                if let Ok(target) = std::fs::read_link(fd.path()) {
                    let t = target.to_string_lossy();
                    // "socket:[12345]"
                    if let Some(inode_str) = t.strip_prefix("socket:[").and_then(|s| s.strip_suffix(']')) {
                        if let Ok(inode) = inode_str.parse::<u64>() {
                            if let Some(&port) = inode_to_port.get(&inode) {
                                result.push((port, pid, String::new()));
                            }
                        }
                    }
                }
            }
        }
    }
    result
}

/// Porte riservate da Nexus e dai suoi servizi di infrastruttura.
/// I processi di progetto NON devono mai usare queste porte.
///
/// Range riservato HTTP:  4000–4079  (microservizi Nexus)
/// Range riservato gRPC:  4100–4139  (canali gRPC interni, target migrazione)
/// Porte gRPC attuali:    50051–50072 (in uso finché non migrati)
/// Progetti utente:       5000+ (assegnate da find_free_port)
pub(super) const NEXUS_RESERVED_PORTS: &[u16] = &[
    // Porte di sistema
    80, 443,
    // ── HTTP Nexus (4000-4079) ─────────────────────────────────────────────
    4000,  // mcp-core HTTP
    4001,  // web-ide (target migrazione da 3000)
    4010,  // admin-service
    4020,  // chat-service
    4030,  // doc-service
    4040,  // billing-service
    4050,  // plugin-service
    4060,  // nexus-gateway
    4070,  // neural-core REST (target migrazione da 8001)
    // ── gRPC interno Nexus (4100-4139, target migrazione) ─────────────────
    4100,  // neural-core gRPC (target da 50051)
    4110,  // tool-runner gRPC (target da 50071)
    4120,  // agent-router gRPC (target da 50072)
    4130,  // presidio gRPC (target da 50052)
    // ── web-ide attuale ───────────────────────────────────────────────────
    3000,  // Nexus web-ide (attuale)
    // ── Porte gRPC attuali (porte alte, in uso finché non migrati) ────────
    8001,  // neural-core REST (attuale)
    50051, // neural-core gRPC
    50052, // presidio gRPC
    50071, // tool-runner gRPC
    50072, // agent-router gRPC
    // ── Database e infrastruttura ─────────────────────────────────────────
    5432, 5433,   // PostgreSQL
    6333, 6334,   // Qdrant REST + gRPC
    6379,         // Redis
    8080,         // nginx interno
    // ── Monitoring e observability ────────────────────────────────────────
    3001,  // Grafana
    4055,  // browser-bridge-mcp
    4317,  // OpenTelemetry Collector gRPC
    4318,  // OpenTelemetry Collector HTTP
    9090,  // Prometheus
    16686, // Jaeger UI
];

/// Range dedicato ai servizi dei progetti gestiti (deve evitare conflitti con Nexus e con servizi host comuni).
/// Scelta conservativa: porte alte non privilegiate, fuori dal range Nexus e fuori dai DB.
pub(super) const PROJECT_PORT_RANGE_START: u16 = 20000;
pub(super) const PROJECT_PORT_RANGE_END: u16 = 39999;
/// Numero porte per progetto nel bucket deterministico.
pub(super) const PROJECT_PORT_BUCKET_SIZE: u16 = 50;

fn project_bucket_start(project_id: &Uuid) -> u16 {
    // Hash stabile: usa i primi 8 byte (big-endian) del UUID.
    let b = project_id.as_bytes();
    let mut v: u64 = 0;
    for i in 0..8 {
        v = (v << 8) | (b[i] as u64);
    }
    let buckets: u64 =
        ((PROJECT_PORT_RANGE_END - PROJECT_PORT_RANGE_START + 1) as u64) / (PROJECT_PORT_BUCKET_SIZE as u64);
    let idx = if buckets == 0 { 0 } else { v % buckets };
    PROJECT_PORT_RANGE_START + (idx as u16) * PROJECT_PORT_BUCKET_SIZE
}

fn stable_hash_u16(input: &str) -> u16 {
    // FNV-1a 32-bit, then fold to u16. Stable across runs/platforms.
    let mut h: u32 = 2166136261;
    for b in input.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(16777619);
    }
    (h ^ (h >> 16)) as u16
}

/// Trova una porta libera *nel bucket deterministico* del progetto.
/// Fallback: se il bucket è pieno (o collisioni esterne), ripiega su `find_free_port(PROJECT_PORT_RANGE_START, ...)`.
pub(super) async fn find_free_project_port(
    project_id: &Uuid,
    registry: &crate::port_registry::PortRegistryCache,
) -> u16 {
    let reserved: std::collections::HashSet<u16> = NEXUS_RESERVED_PORTS.iter().copied().collect();
    let allocated: std::collections::HashSet<u16> = registry
        .current()
        .await
        .all_allocated_ports()
        .into_iter()
        .collect();

    let start = project_bucket_start(project_id);
    let end = (start + PROJECT_PORT_BUCKET_SIZE).saturating_sub(1);

    let mut port = start;
    while port <= end {
        if !reserved.contains(&port) && !allocated.contains(&port) {
            if tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port)).await.is_ok() {
                return port;
            }
        }
        port += 1;
    }

    // Bucket pieno o tutte occupate: fallback su scan globale nel range progetti.
    find_free_port(PROJECT_PORT_RANGE_START, registry).await
}

/// Porta deterministica per un dato servizio/config all'interno del bucket del progetto.
/// Usa `service_key` come input stabile (es. label o short name) e linear-probing nel bucket.
pub(super) async fn deterministic_project_port_for_key(
    project_id: &Uuid,
    service_key: &str,
    registry: &crate::port_registry::PortRegistryCache,
) -> u16 {
    let start = project_bucket_start(project_id);
    let end = (start + PROJECT_PORT_BUCKET_SIZE).saturating_sub(1);
    let reserved: std::collections::HashSet<u16> = NEXUS_RESERVED_PORTS.iter().copied().collect();
    let allocated: std::collections::HashSet<u16> = registry
        .current()
        .await
        .all_allocated_ports()
        .into_iter()
        .collect();

    let mut offset = stable_hash_u16(service_key) % PROJECT_PORT_BUCKET_SIZE;
    let mut tries: u16 = 0;
    while tries < PROJECT_PORT_BUCKET_SIZE {
        let port = start.saturating_add(offset);
        if port >= start && port <= end && !reserved.contains(&port) && !allocated.contains(&port) {
            if tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port)).await.is_ok() {
                return port;
            }
        }
        offset = (offset + 1) % PROJECT_PORT_BUCKET_SIZE;
        tries += 1;
    }
    find_free_project_port(project_id, registry).await
}

/// Trova la prima porta TCP libera a partire da `start`, escludendo le porte
/// riservate da Nexus E quelle gia' allocate nel registro centralizzato.
pub(super) async fn find_free_port(start: u16, registry: &crate::port_registry::PortRegistryCache) -> u16 {
    let reserved: std::collections::HashSet<u16> = NEXUS_RESERVED_PORTS.iter().copied().collect();
    let allocated: std::collections::HashSet<u16> = registry
        .current()
        .await
        .all_allocated_ports()
        .into_iter()
        .collect();
    let mut port = start;
    loop {
        if port > 65000 { return start; } // fallback di sicurezza
        if reserved.contains(&port) || allocated.contains(&port) { port += 1; continue; }
        // Evita assegnazioni fuori dal range progetti quando start è nel range progetti.
        if start >= PROJECT_PORT_RANGE_START && start <= PROJECT_PORT_RANGE_END {
            if port < PROJECT_PORT_RANGE_START || port > PROJECT_PORT_RANGE_END {
                port = PROJECT_PORT_RANGE_START;
                continue;
            }
        }
        if tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port)).await.is_ok() {
            return port;
        }
        port += 1;
    }
}

/// Versione senza registry (legacy, per contesti dove il registry non e' disponibile).
/// Da usare solo in test o during bootstrap.
#[allow(dead_code)]
pub(super) async fn find_free_port_no_registry(start: u16) -> u16 {
    let reserved: std::collections::HashSet<u16> = NEXUS_RESERVED_PORTS.iter().copied().collect();
    let mut port = start;
    loop {
        if port > 65000 { return start; }
        if reserved.contains(&port) { port += 1; continue; }
        if tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port)).await.is_ok() {
            return port;
        }
        port += 1;
    }
}

/// Restituisce true se lo script npm/pnpm/yarn è probabilmente un web server
/// (quindi ha bisogno di una porta).
pub(crate) fn is_web_service_script(script_name: &str) -> bool {
    matches!(script_name, "dev" | "start" | "serve" | "preview")
}

/// Prima di avviare un servizio systemd, estrae le porte dal file .service
/// (Environment= e ExecStart) e libera quelle occupate da processi estranei
/// (inclusi container Docker). Ritorna le porte effettivamente liberate.
async fn free_ports_for_unit(unit_name: &str) -> Vec<serde_json::Value> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let unit_path = format!("{}/.config/systemd/user/{}", home, unit_name);
    let unit_content = match tokio::fs::read_to_string(&unit_path).await {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let ports = extract_ports_from_unit(&unit_content);
    if ports.is_empty() {
        return Vec::new();
    }

    let own_pid = get_service_main_pid(unit_name).await;

    let listening = read_listening_ports_ss().await
        .unwrap_or_else(|_| read_listening_ports_proc());

    let mut freed = Vec::new();
    for target_port in &ports {
        for &(port, pid, ref program) in &listening {
            if port != *target_port { continue; }
            if pid == 0 { continue; }
            if Some(pid) == own_pid { continue; }
            let _ = tokio::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .output()
                .await;
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if std::path::Path::new(&format!("/proc/{}", pid)).exists() {
                let _ = tokio::process::Command::new("kill")
                    .args(["-KILL", &pid.to_string()])
                    .output()
                    .await;
            }
            freed.push(json!({
                "port": port,
                "pid": pid,
                "program": program,
                "method": "kill",
            }));
            tracing::info!("Porta {} liberata: terminato PID {} ({}) per avvio {}", port, pid, program, unit_name);
        }

        // Container Docker che occupano questa porta
        if let Ok(docker_out) = tokio::process::Command::new("docker")
            .args(["ps", "--format", "{{.Names}}|{{.Ports}}"])
            .output()
            .await
        {
            let docker_str = String::from_utf8_lossy(&docker_out.stdout);
            for line in docker_str.lines() {
                let parts: Vec<&str> = line.splitn(2, '|').collect();
                if parts.len() != 2 { continue; }
                let cname = parts[0].trim();
                let port_section = parts[1];
                let occupies_port = port_section.split(',').any(|entry| {
                    if let Some(arrow_pos) = entry.find("->") {
                        let host_part = &entry[..arrow_pos];
                        host_part.rsplit(':').next()
                            .and_then(|p| p.trim().parse::<u16>().ok())
                            .map_or(false, |p| p == *target_port)
                    } else {
                        false
                    }
                });
                if occupies_port {
                    let _ = tokio::process::Command::new("docker")
                        .args(["stop", "-t", "5", cname])
                        .output()
                        .await;
                    freed.push(json!({
                        "port": target_port,
                        "container": cname,
                        "method": "docker stop",
                    }));
                    tracing::info!("Porta {} liberata: fermato container Docker '{}' per avvio {}", target_port, cname, unit_name);
                }
            }
        }
    }
    freed
}

pub(super) fn extract_ports_from_unit(content: &str) -> Vec<u16> {
    let mut ports = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Environment=") {
            // Es: PORT=5215, ASPNETCORE_URLS=http://+:5215, SERVER_PORT=8080
            for segment in rest.split_whitespace() {
                if let Some(val) = segment.split('=').nth(1) {
                    // Porta diretta (es. PORT=5215)
                    if let Ok(p) = val.parse::<u16>() {
                        if p > 0 { ports.push(p); continue; }
                    }
                    // URL con porta (es. http://+:5215 o http://0.0.0.0:5215)
                    for part in val.split(';') {
                        if let Some(colon_pos) = part.rfind(':') {
                            let after = &part[colon_pos + 1..];
                            let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
                            if let Ok(p) = num_str.parse::<u16>() {
                                if p > 0 { ports.push(p); }
                            }
                        }
                    }
                }
            }
        }
        if let Some(rest) = line.strip_prefix("ExecStart=") {
            // Pattern: --port 5215, -p 5215, --urls http://+:5215
            let tokens: Vec<&str> = rest.split_whitespace().collect();
            for (i, tok) in tokens.iter().enumerate() {
                if (*tok == "--port" || *tok == "-p" || *tok == "--server.port")
                    && i + 1 < tokens.len()
                {
                    if let Ok(p) = tokens[i + 1].parse::<u16>() {
                        if p > 0 { ports.push(p); }
                    }
                }
                if *tok == "--urls" && i + 1 < tokens.len() {
                    if let Some(colon_pos) = tokens[i + 1].rfind(':') {
                        let after = &tokens[i + 1][colon_pos + 1..];
                        let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
                        if let Ok(p) = num_str.parse::<u16>() {
                            if p > 0 { ports.push(p); }
                        }
                    }
                }
                // --port=5215
                if tok.starts_with("--port=") || tok.starts_with("-p=") {
                    if let Some(val) = tok.split('=').nth(1) {
                        if let Ok(p) = val.parse::<u16>() {
                            if p > 0 { ports.push(p); }
                        }
                    }
                }
            }
        }
    }
    ports.sort();
    ports.dedup();
    ports
}

// ── Diagnostica crash-loop ──────────────────────────────────────────────────

struct ServiceDiagnosis {
    error: String,
    suggestion: String,
    kind: &'static str,
}

fn diagnose_service_failure(log: &str, _unit: &str, root: &std::path::Path) -> ServiceDiagnosis {
    let log_lc = log.to_lowercase();

    // 1. Script npm mancante
    if log_lc.contains("missing script:") {
        let script = log.lines()
            .find(|l| l.to_lowercase().contains("missing script:"))
            .and_then(|l| l.split('"').nth(1))
            .unwrap_or("sconosciuto");
        return ServiceDiagnosis {
            error: format!("Lo script npm '{}' non esiste nel package.json", script),
            suggestion: "Verifica che il package.json contenga lo script corretto nella sezione \"scripts\". Prova a eseguire 'Rianalizza progetto' dal pannello Source Control.".into(),
            kind: "missing_script",
        };
    }

    // 2. Directory di lavoro non trovata
    if log_lc.contains("changing to the requested working directory failed")
        || log_lc.contains("no such file or directory")
        || log_lc.contains("chdir")
    {
        return ServiceDiagnosis {
            error: "La directory di lavoro del servizio non esiste".into(),
            suggestion: "Il progetto potrebbe essere incompleto. Disinstalla il servizio e usa '+ Configura' per ricrearlo dopo aver verificato la struttura del progetto.".into(),
            kind: "missing_directory",
        };
    }

    // 3. Dipendenze node mancanti
    if log_lc.contains("cannot find module")
        || log_lc.contains("module not found")
        || log_lc.contains("err_module_not_found")
    {
        // Verifica se node_modules esiste
        let has_node_modules = root.join("node_modules").exists()
            || std::fs::read_dir(root).ok()
                .map(|d| d.flatten().any(|e| e.path().is_dir() && e.path().join("node_modules").exists()))
                .unwrap_or(false);
        let suggestion = if has_node_modules {
            "Un modulo non e' installato. Esegui 'npm install' o 'pnpm install' nel terminale del progetto."
        } else {
            "Le dipendenze non sono installate. Esegui 'npm install' nel terminale del progetto prima di avviare il servizio."
        };
        return ServiceDiagnosis {
            error: "Modulo Node.js non trovato — dipendenze mancanti".into(),
            suggestion: suggestion.into(),
            kind: "missing_dependencies",
        };
    }

    // 4. SDK .NET mancante
    if log_lc.contains("dotnet") && (log_lc.contains("not found") || log_lc.contains("command not found")) {
        return ServiceDiagnosis {
            error: "Il .NET SDK non e' installato o non e' nel PATH".into(),
            suggestion: "Installa il .NET SDK con 'sudo apt install dotnet-sdk-9.0' oppure usa la versione Docker del servizio.".into(),
            kind: "missing_sdk",
        };
    }

    // 5. Build .NET fallita
    if log_lc.contains("build failed") || log_lc.contains("msbuild") && log_lc.contains("error") {
        return ServiceDiagnosis {
            error: "La build .NET e' fallita".into(),
            suggestion: "Esegui 'dotnet build' manualmente nel terminale per vedere gli errori di compilazione.".into(),
            kind: "build_failed",
        };
    }

    // 6. Porta occupata
    if log_lc.contains("address already in use") || log_lc.contains("eaddrinuse") {
        return ServiceDiagnosis {
            error: "La porta richiesta e' gia' occupata da un altro processo".into(),
            suggestion: "Usa il pulsante 'X Porte' per liberare le porte conflittuali, poi riavvia il servizio.".into(),
            kind: "port_in_use",
        };
    }

    // 7. Permessi insufficienti
    if log_lc.contains("permission denied") || log_lc.contains("eacces") {
        return ServiceDiagnosis {
            error: "Permessi insufficienti per eseguire il servizio".into(),
            suggestion: "Verifica i permessi dei file del progetto. Potresti dover eseguire 'chmod +x' sul file eseguibile.".into(),
            kind: "permission_denied",
        };
    }

    // 8. Fallback: mostra le ultime righe del log
    let last_lines: Vec<&str> = log.lines()
        .filter(|l| {
            let ll = l.to_lowercase();
            ll.contains("error") || ll.contains("fail") || ll.contains("exception")
                || ll.contains("fatal") || ll.contains("panic")
        })
        .collect();
    let error_summary = if last_lines.is_empty() {
        log.lines().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" | ")
    } else {
        last_lines.into_iter().take(3).collect::<Vec<_>>().join(" | ")
    };

    ServiceDiagnosis {
        error: if error_summary.is_empty() {
            "Il servizio si arresta ripetutamente (causa sconosciuta)".into()
        } else {
            error_summary
        },
        suggestion: "Controlla i log completi nel tab Terminale con 'journalctl --user -u <servizio> -n 50'.".into(),
        kind: "unknown",
    }
}

async fn get_service_main_pid(unit_name: &str) -> Option<u32> {
    let out = tokio::process::Command::new("systemctl")
        .args(["--user", "show", unit_name, "--property=MainPID"])
        .output()
        .await
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    for line in s.lines() {
        if let Some(val) = line.strip_prefix("MainPID=") {
            if let Ok(pid) = val.trim().parse::<u32>() {
                if pid > 0 { return Some(pid); }
            }
        }
    }
    None
}

// ── API Gestione Porte Allocate ──────────────────────────────────────────────

/// GET /api/projects/:id/port-allocations
/// Lista tutte le porte allocate al progetto.
pub async fn get_port_allocations(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    let allocations = state.port_registry.ports_for_project(&project_id).await;
    let items: Vec<Value> = allocations
        .iter()
        .map(|a| {
            json!({
                "id": a.id.to_string(),
                "port": a.port,
                "label": a.label,
                "allocation_mode": a.allocation_mode,
                "run_config_id": a.run_config_id.map(|u| u.to_string()),
                "service_unit": a.service_unit,
            })
        })
        .collect();

    Ok(Json(json!({ "allocations": items })))
}

/// POST /api/projects/:id/port-allocations
/// Alloca una porta al progetto. Body JSON: { port, label?, mode? "manual"|"auto", run_config_id?, service_unit? }
pub async fn create_port_allocation(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    let port = body["port"]
        .as_u64()
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Campo 'port' obbligatorio (numero)"))?
        as u16;

    // Validazione range
    if port < 1024 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Porta deve essere >= 1024 (porte privilegiate non ammesse)",
        ));
    }

    // Controllo porte riservate Nexus
    let reserved: std::collections::HashSet<u16> = NEXUS_RESERVED_PORTS.iter().copied().collect();
    if reserved.contains(&port) {
        return Err(api_error(
            StatusCode::CONFLICT,
            format!("Porta {} riservata ai servizi interni Nexus", port),
        ));
    }

    let label = body["label"].as_str().unwrap_or("");
    let mode = body["mode"].as_str().unwrap_or("manual");
    if mode != "auto" && mode != "manual" {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Campo 'mode' deve essere 'auto' o 'manual'",
        ));
    }

    let run_config_id = body["run_config_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok());
    let service_unit = body["service_unit"].as_str();

    match state
        .port_registry
        .allocate(project_id, port, label, mode, run_config_id, service_unit)
        .await
    {
        Ok(alloc) => Ok(Json(json!({
            "ok": true,
            "allocation": {
                "id": alloc.id.to_string(),
                "port": alloc.port,
                "label": alloc.label,
                "allocation_mode": alloc.allocation_mode,
            }
        }))),
        Err(e) => Err(api_error(StatusCode::CONFLICT, e)),
    }
}

/// DELETE /api/projects/:id/port-allocations/:port
/// Rilascia una porta allocata al progetto.
pub async fn delete_port_allocation(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((id, port_str)): AxumPath<(String, String)>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let _project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    let port: u16 = port_str
        .parse()
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Porta non valida"))?;

    // Verifica che la porta appartenga effettivamente al progetto
    let registry = state.port_registry.current().await;
    if let Some(alloc) = registry.by_port.get(&port) {
        if alloc.project_id != _project_id {
            return Err(api_error(
                StatusCode::FORBIDDEN,
                "Porta allocata a un altro progetto",
            ));
        }
    } else {
        return Err(api_error(StatusCode::NOT_FOUND, "Porta non allocata"));
    }
    drop(registry);

    state
        .port_registry
        .release(port)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(json!({ "ok": true })))
}
