use super::*;

/// Analizza il filesystem del progetto e suggerisce definizioni di servizi systemd.
/// Riconosce: npm/pnpm scripts, Cargo binaries, .csproj / launchSettings.json,
/// docker-compose.yml, python app entry points.
pub async fn wizard_detect_services(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let root = context.root_path.to_string_lossy().to_string();
    let slug = context.details.name.to_lowercase().replace([' ', '_'], "-");

    let mut suggestions: Vec<serde_json::Value> = Vec::new();

    // ── 1. package.json / pnpm ─────────────────────────────────────────────
    let pkg_paths = find_files_named(&root, "package.json", 6).await;
    for pkg_path in &pkg_paths {
        if let Ok(content) = tokio::fs::read_to_string(pkg_path).await {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&content) {
                let scripts = pkg.get("scripts").and_then(|s| s.as_object());
                let cwd = std::path::Path::new(pkg_path).parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| root.clone());
                let rel = cwd.strip_prefix(&root).unwrap_or("").trim_start_matches('/');
                let pkg_manager = if tokio::fs::metadata(format!("{}/pnpm-lock.yaml", cwd)).await.is_ok() {
                    "pnpm"
                } else {
                    "npm"
                };
                for script_name in ["dev", "start", "serve", "preview"] {
                    if scripts.map(|s| s.contains_key(script_name)).unwrap_or(false) {
                        let svc_short = if rel.is_empty() {
                            script_name.to_string()
                        } else {
                            format!("{}-{}", rel.replace('/', "-"), script_name)
                        };
                        suggestions.push(json!({
                            "short":    svc_short,
                            "unit":     format!("{}-{}.service", slug, svc_short),
                            "label":    format!("{} {} ({})", pkg_manager, script_name, if rel.is_empty() { "root" } else { rel }),
                            "kind":     if pkg_manager == "pnpm" { "pnpm" } else { "npm" },
                            "command":  pkg_manager,
                            "args":     ["run", script_name],
                            "cwd":      cwd,
                            "existing": false,
                        }));
                        break; // un solo script per package.json
                    }
                }
            }
        }
    }

    // ── 2. .NET / launchSettings.json ──────────────────────────────────────
    let launch_paths = find_files_named(&root, "launchSettings.json", 8).await;
    for lp in &launch_paths {
        let cwd = std::path::Path::new(lp).parent()
            .and_then(|p| p.parent()) // Properties/ → project dir
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| root.clone());
        let csproj = find_csproj_in(&cwd).await;
        let proj_arg = csproj.as_deref().unwrap_or(".");
        let rel = cwd.strip_prefix(&root).unwrap_or("").trim_start_matches('/');
        let svc_short = if rel.is_empty() { "dotnet".to_string() }
                        else { rel.replace('/', "-") };
        suggestions.push(json!({
            "short":    svc_short,
            "unit":     format!("{}-{}.service", slug, svc_short),
            "label":    format!("dotnet run ({})", if rel.is_empty() { "root" } else { rel }),
            "kind":     "dotnet",
            "command":  "dotnet",
            "args":     ["run", "--project", proj_arg],
            "cwd":      cwd,
            "existing": false,
        }));
    }

    // ── 3. Cargo.toml binaries ─────────────────────────────────────────────
    let cargo_paths = find_files_named(&root, "Cargo.toml", 6).await;
    for cp in &cargo_paths {
        if let Ok(content) = tokio::fs::read_to_string(cp).await {
            // Cerca [[bin]] entries
            let bin_names: Vec<String> = content.lines()
                .filter_map(|l| {
                    let t = l.trim();
                    if t.starts_with("name") && content.contains("[[bin]]") {
                        t.splitn(2, '=').nth(1)
                            .map(|v| v.trim().trim_matches('"').to_string())
                    } else { None }
                }).collect();
            let cwd = std::path::Path::new(cp).parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| root.clone());
            let rel = cwd.strip_prefix(&root).unwrap_or("").trim_start_matches('/');
            for bin in &bin_names {
                let svc_short = format!("cargo-{}", bin);
                suggestions.push(json!({
                    "short":    svc_short,
                    "unit":     format!("{}-{}.service", slug, svc_short),
                    "label":    format!("cargo run --bin {} ({})", bin, if rel.is_empty() { "root" } else { rel }),
                    "kind":     "cargo",
                    "command":  "cargo",
                    "args":     ["run", "--bin", bin],
                    "cwd":      cwd,
                    "existing": false,
                }));
            }
        }
    }

    // ── 4. docker-compose.yml ──────────────────────────────────────────────
    for dc_name in &["docker-compose.yml", "docker-compose.yaml",
                     "docker-compose.dev.yml", "docker-compose.dev.yaml"] {
        let dc_path = format!("{}/{}", root, dc_name);
        if tokio::fs::metadata(&dc_path).await.is_ok() {
            suggestions.push(json!({
                "short":    "docker",
                "unit":     format!("{}-docker.service", slug),
                "label":    format!("docker compose up ({})", dc_name),
                "kind":     "shell",
                "command":  "docker",
                "args":     ["compose", "-f", dc_name, "up"],
                "cwd":      root,
                "existing": false,
            }));
            break;
        }
    }

    // ── 5. Python entry points ─────────────────────────────────────────────
    for py_entry in &["main.py", "app.py", "server.py", "run.py", "manage.py"] {
        let py_path = format!("{}/{}", root, py_entry);
        if tokio::fs::metadata(&py_path).await.is_ok() {
            let svc_short = py_entry.strip_suffix(".py").unwrap_or(py_entry);
            suggestions.push(json!({
                "short":    svc_short,
                "unit":     format!("{}-{}.service", slug, svc_short),
                "label":    format!("python {} (root)", py_entry),
                "kind":     "python",
                "command":  "python3",
                "args":     [py_entry],
                "cwd":      root,
                "existing": false,
            }));
        }
    }

    // Marca quelli già installati come .service files
    if let Ok(svc_out) = tokio::process::Command::new("systemctl")
        .args(["--user", "list-unit-files", "--type=service", "--no-legend", "--no-pager"])
        .output().await
    {
        let installed: std::collections::HashSet<String> = String::from_utf8_lossy(&svc_out.stdout)
            .lines()
            .filter_map(|l| l.split_whitespace().next().map(String::from))
            .collect();
        for s in &mut suggestions {
            let unit = s["unit"].as_str().unwrap_or("").to_string();
            if installed.contains(&unit) {
                s["existing"] = json!(true);
            }
        }
    }

    Ok(Json(json!({ "suggestions": suggestions, "slug": slug })))
}

/// Installa un servizio come unit file systemd --user e lo abilita.
/// Body JSON: { short, command, args, cwd, env, description }
pub async fn wizard_install_service(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let slug = context.details.name.to_lowercase().replace([' ', '_'], "-");

    let short = body["short"].as_str()
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Campo 'short' obbligatorio"))?;
    let command = body["command"].as_str()
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Campo 'command' obbligatorio"))?;
    let root_str = context.root_path.to_string_lossy().to_string();
    let cwd = body["cwd"].as_str().unwrap_or(&root_str);
    let desc_fallback = format!("{} {}", context.details.name, short);
    let description = body["description"].as_str().unwrap_or(&desc_fallback);

    if short.contains('/') || short.contains("..") {
        return Err(api_error(StatusCode::BAD_REQUEST, "Nome servizio non valido"));
    }

    // ── Validazione anti-placeholder ─────────────────────────────────────
    // Rifiuta comandi vuoti o no-op come `/bin/true`, `/bin/false`, `:`, `true`,
    // `false`, `sleep`, ecc. — generavano servizi "fantasma" che apparivano
    // sempre `active (exited)` ma non facevano nulla, confondendo l'utente.
    let cmd_trim = command.trim();
    if cmd_trim.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "Il comando del servizio non può essere vuoto"));
    }
    let cmd_basename = std::path::Path::new(cmd_trim)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(cmd_trim);
    const FORBIDDEN_NOOP: &[&str] = &[
        "true", "false", ":", "sleep", "echo", "exit", "noop", "no-op",
    ];
    if FORBIDDEN_NOOP.contains(&cmd_basename) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            format!(
                "Il comando '{}' è un no-op che genererebbe un servizio segnaposto. \
                 Specifica un comando reale (es. 'pnpm', 'dotnet', 'docker', un path eseguibile) \
                 o, se non hai ancora il comando, NON installare ora il servizio.",
                cmd_basename
            ),
        ));
    }

    // ── Validazione cwd: deve esistere, altrimenti il servizio fallirà al primo start ──
    if let Err(e) = tokio::fs::metadata(cwd).await {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            format!("La directory di lavoro '{}' non esiste o non è accessibile: {}", cwd, e),
        ));
    }

    // Sicurezza: il nome unit deve iniziare col prefisso slug
    let unit_name = format!("{}-{}.service", slug, short);

    // Costruisce ExecStart.
    //
    // BUG fix systemd 203/EXEC: systemd --user NON eredita il PATH della shell
    // utente per la risoluzione del binary in ExecStart, e Environment=PATH=...
    // viene applicato solo dopo l'exec (limitazione documentata di systemd).
    // Quindi binary in ~/.dotnet, ~/.cargo/bin, ~/.local/bin causano 203/EXEC
    // se scriviamo `ExecStart=dotnet run ...` (binary nudo).
    //
    // Soluzione: risolvi il path assoluto del binary via bash login shell
    // (`bash -lc 'command -v X'`) che eredita il PATH dell'utente, e usalo
    // in ExecStart. Se il binary inizia gia' con / o ./ lo lasciamo invariato.
    let args: Vec<String> = body["args"].as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let resolved_command: String = if command.starts_with('/') || command.starts_with("./") {
        // Path gia' assoluto/relativo esplicito
        command.to_string()
    } else {
        // Binary nudo: risolvi al path assoluto in 2 step.
        //
        // 1. Tentativo via login shell `bash -lc 'command -v X'` (rispetta
        //    eventuali config personalizzate dell'utente).
        // 2. Fallback su lista di prefissi tipici per binary "user-installed":
        //    ~/.dotnet, ~/.cargo/bin, ~/.local/bin, /usr/local/bin, ecc.
        //    Necessario perche' `.bashrc` di alcuni utenti non viene caricato
        //    da shell non-interactive, oppure punta a path errati.
        // 3. Se neanche cosi' troviamo, errore esplicito.
        let probed: Option<String> = {
            let r = tokio::process::Command::new("/bin/bash")
                .args(["-lc", &format!("command -v {}", command)])
                .output().await;
            match r {
                Ok(out) if out.status.success() => {
                    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if p.is_empty() { None } else { Some(p) }
                }
                _ => None,
            }
        };

        let resolved = match probed {
            Some(p) => p,
            None => {
                // Fallback su path tipici. HOME e' rilasciato qui (gia' usato sopra)
                let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
                let candidates = vec![
                    format!("{}/.dotnet/{}", home, command),
                    format!("{}/.cargo/bin/{}", home, command),
                    format!("{}/.local/bin/{}", home, command),
                    format!("/usr/local/bin/{}", command),
                    format!("/usr/bin/{}", command),
                    format!("/bin/{}", command),
                ];
                let mut found: Option<String> = None;
                for cand in &candidates {
                    if tokio::fs::metadata(cand).await.is_ok() {
                        found = Some(cand.clone());
                        break;
                    }
                }
                match found {
                    Some(p) => p,
                    None => {
                        return Err(api_error(
                            StatusCode::BAD_REQUEST,
                            format!(
                                "Binary '{}' non trovato. Cercato in: bash login PATH + {}. \
                                 Installa il tool o specifica il path assoluto nel comando.",
                                command,
                                candidates.join(", ")
                            ),
                        ));
                    }
                }
            }
        };
        resolved
    };

    let exec_start = if args.is_empty() {
        resolved_command
    } else {
        format!("{} {}", resolved_command, args.join(" "))
    };

    fn parse_port_token(s: &str) -> Option<u16> {
        let t = s.trim();
        if t.is_empty() { return None; }
        let t = t.trim_matches(|c: char| c == '"' || c == '\'' || c == ',' || c == ';');
        t.parse::<u16>().ok()
    }

    fn looks_like_web_server_command(command: &str) -> bool {
        let lower = command.to_lowercase();
        lower.contains(" next dev")
            || lower.contains(" next start")
            || lower.contains(" vite")
            || lower.contains(" nuxt")
            || lower.contains(" astro")
            || lower.contains(" react-scripts start")
            || lower.contains(" pnpm run dev")
            || lower.contains(" npm run dev")
            || lower.contains(" yarn dev")
            || lower.contains(" pnpm dev")
            || lower.contains(" npm start")
            || lower.contains(" dotnet run")
    }

    fn rewrite_port_flags(command: &str, target_port: u16) -> String {
        let p = target_port.to_string();
        let mut out = command.to_string();
        for bad in ["3000", "4000", "4010", "4020", "4030", "4040", "4050", "4060", "8001"] {
            out = out.replace(&format!("--port={}", bad), &format!("--port={}", p));
            out = out.replace(&format!("--port {}", bad), &format!("--port {}", p));
            out = out.replace(&format!("-p {}", bad), &format!("-p {}", p));
            out = out.replace(&format!("-p{}", bad), &format!("-p{}", p));
            out = out.replace(&format!("localhost:{}", bad), &format!("localhost:{}", p));
            out = out.replace(&format!("127.0.0.1:{}", bad), &format!("127.0.0.1:{}", p));
        }
        let lower = out.to_lowercase();
        let has_flag = lower.contains("--port") || lower.split_whitespace().any(|t| t == "-p" || t.starts_with("-p"));
        if (lower.contains("next dev") || lower.contains("next start")) && !has_flag {
            out.push_str(&format!(" -p {}", p));
        }
        out
    }

    // Blocco Environment= per variabili d'ambiente (con policy porte: mai usare porte riservate Nexus, incl. 3000).
    let reserved: std::collections::HashSet<u16> = services::NEXUS_RESERVED_PORTS.iter().copied().collect();
    let mut env_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if let Some(obj) = body["env"].as_object() {
        for (k, v) in obj {
            env_map.insert(k.clone(), v.as_str().unwrap_or("").to_string());
        }
    }

    let wants_port = looks_like_web_server_command(&exec_start);
    let existing_port = env_map.get("PORT").and_then(|v| parse_port_token(v));
    let final_port = if wants_port {
        let port = existing_port.unwrap_or_else(|| 5000);
        let ok = !reserved.contains(&port)
            && state.port_registry.is_port_available(port).await
            && tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port)).await.is_ok();
        let actual = if ok { port } else { services::find_free_project_port(&project_id, &state.port_registry).await };
        env_map.insert("PORT".to_string(), actual.to_string());
        Some(actual)
    } else {
        None
    };

    let exec_start = if let Some(p) = final_port {
        rewrite_port_flags(&exec_start, p)
    } else {
        exec_start
    };

    let env_lines: String = env_map
        .iter()
        .map(|(k, v)| format!("Environment={}={}\n", k, v))
        .collect();

    let unit_content = format!(
        "[Unit]\nDescription={}\nAfter=network.target\n\n[Service]\nType=simple\nWorkingDirectory={}\n{}ExecStart={}\nRestart=on-failure\nRestartSec=5\nStandardOutput=journal\nStandardError=journal\n\n[Install]\nWantedBy=default.target\n",
        description, cwd, env_lines, exec_start
    );

    // Scrive il file nella directory systemd --user
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let svc_dir = format!("{}/.config/systemd/user", home);
    let svc_path = format!("{}/{}", svc_dir, unit_name);

    tokio::fs::create_dir_all(&svc_dir).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("mkdir: {}", e)))?;
    tokio::fs::write(&svc_path, &unit_content).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("write: {}", e)))?;

    // Cleanup: rimuove servizi disabled dello stesso progetto con ruolo sovrapponibile.
    // Es. se stiamo installando "backend-FreeLance.Api", rimuove "backend" (disabled)
    // perche' il nome corto del vecchio servizio e' un prefisso del nuovo.
    let mut cleaned: Vec<String> = Vec::new();
    let slug_prefix = format!("{}-", slug);
    if let Ok(list_out) = tokio::process::Command::new("systemctl")
        .args(["--user", "list-unit-files", "--type=service", "--no-legend", "--no-pager"])
        .output()
        .await
    {
        for line in String::from_utf8_lossy(&list_out.stdout).lines() {
            let cols: Vec<&str> = line.split_whitespace().collect();
            let old_unit = cols.first().copied().unwrap_or("");
            let old_state = cols.get(1).copied().unwrap_or("");
            if old_state != "disabled" { continue; }
            if !old_unit.starts_with(&slug_prefix) || !old_unit.ends_with(".service") { continue; }
            if old_unit == unit_name { continue; }
            let old_short = old_unit
                .strip_prefix(&slug_prefix).unwrap_or(old_unit)
                .strip_suffix(".service").unwrap_or(old_unit);
            if short.starts_with(old_short) || old_short.starts_with(short) {
                let old_path = format!("{}/{}", svc_dir, old_unit);
                let _ = tokio::process::Command::new("systemctl")
                    .args(["--user", "stop", old_unit]).output().await;
                let _ = tokio::process::Command::new("systemctl")
                    .args(["--user", "disable", old_unit]).output().await;
                let _ = tokio::fs::remove_file(&old_path).await;
                cleaned.push(old_unit.to_string());
                tracing::info!("Rimosso servizio orfano {} (sostituito da {})", old_unit, unit_name);
            }
        }
    }

    // daemon-reload + enable
    let _ = tokio::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"]).output().await;
    let enable_out = tokio::process::Command::new("systemctl")
        .args(["--user", "enable", &unit_name]).output().await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Registra le porte del servizio nel port_registry (mig 0114).
    // Estrae le porte dal contenuto unit appena scritto e le registra come "auto".
    let detected_ports = services::extract_ports_from_unit(&unit_content);
    for p in &detected_ports {
        // Ignora errori di registrazione (es. porta gia' allocata) — non blocca l'install
        if let Err(e) = state.port_registry.allocate(
            project_id, *p, short, "auto", None, Some(&unit_name),
        ).await {
            tracing::warn!("port_registry: registrazione porta {} per {} fallita: {}", p, unit_name, e);
        }
    }

    Ok(Json(json!({
        "ok":      enable_out.status.success(),
        "unit":    unit_name,
        "path":    svc_path,
        "content": unit_content,
        "cleaned": cleaned,
    })))
}

// ── DELETE /api/projects/:id/services/:service ───────────────────────────────
/// Disinstalla un servizio systemd `{slug}-{service}.service` del progetto:
/// stop + disable + rimuove il file `~/.config/systemd/user/<unit>` + daemon-reload.
/// Sicurezza: il nome unit risultante DEVE iniziare con `{slug}-`.
pub async fn uninstall_project_service(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((id, service)): AxumPath<(String, String)>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;
    let slug = context.details.name.to_lowercase().replace([' ', '_'], "-");

    if service.contains('/') || service.contains("..") {
        return Err(api_error(StatusCode::BAD_REQUEST, "Nome servizio non valido"));
    }
    let unit_name = if service.starts_with(&format!("{}-", slug)) {
        if service.ends_with(".service") { service.clone() } else { format!("{}.service", service) }
    } else {
        format!("{}-{}.service", slug, service)
    };
    // Sicurezza ridondante
    if !unit_name.starts_with(&format!("{}-", slug)) {
        return Err(api_error(StatusCode::FORBIDDEN, "L'unit non appartiene al progetto"));
    }

    // 1. stop (ignora errori: il servizio potrebbe già essere fermo)
    let _ = tokio::process::Command::new("systemctl")
        .args(["--user", "stop", &unit_name]).output().await;
    // 2. disable
    let _ = tokio::process::Command::new("systemctl")
        .args(["--user", "disable", &unit_name]).output().await;

    // 3. Prima di rimuovere il file, leggi il contenuto per estrarre le porte da rilasciare
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let svc_path = format!("{}/.config/systemd/user/{}", home, unit_name);

    // Rilascia porte dal port_registry leggendo il file prima di cancellarlo
    if let Ok(content) = tokio::fs::read_to_string(&svc_path).await {
        let ports = services::extract_ports_from_unit(&content);
        for p in ports {
            if let Err(e) = state.port_registry.release(p).await {
                tracing::debug!("port_registry: rilascio porta {} per {} ignorato: {}", p, unit_name, e);
            }
        }
    }

    // 4. Rimozione del file unit
    let removed = match tokio::fs::remove_file(&svc_path).await {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Impossibile rimuovere {}: {}", svc_path, e),
        )),
    };

    // 5. daemon-reload
    let _ = tokio::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"]).output().await;

    Ok(Json(json!({
        "ok":      true,
        "unit":    unit_name,
        "path":    svc_path,
        "removed": removed,
    })))
}

// Helpers per wizard_detect_services ──────────────────────────────────────

/// Cerca ricorsivamente (BFS iterativo) file con un dato nome fino a max_depth livelli.
/// Salta le directory irrilevanti per velocizzare la ricerca.
pub(super) async fn find_files_named(root: &str, filename: &str, max_depth: usize) -> Vec<String> {
    // Directory sempre da saltare: non contengono sorgenti propri del progetto
    const SKIP: &[&str] = &[
        ".git", "node_modules", ".next", ".turbo", ".cache",
        "__pycache__", ".venv", "venv", "env",
        "obj", "bin",            // .NET build output
        ".terraform", ".gradle", // build tools
        "vendor",                // Go/PHP vendor
    ];
    // Salta "target" solo se contiene a sua volta "debug" o "release" (indice di build Rust)
    let mut results = Vec::new();
    let mut queue: std::collections::VecDeque<(std::path::PathBuf, usize)> =
        std::collections::VecDeque::new();
    queue.push_back((std::path::PathBuf::from(root), 0));

    while let Some((dir, depth)) = queue.pop_front() {
        let Ok(mut rd) = tokio::fs::read_dir(&dir).await else { continue };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if SKIP.contains(&name) { continue; }
            // Salta "target/" solo se sembra una build Rust (ha "debug" o "release" al suo interno)
            if name == "target" && path.is_dir() {
                let is_rust_target =
                    tokio::fs::metadata(path.join("debug")).await.is_ok() ||
                    tokio::fs::metadata(path.join("release")).await.is_ok();
                if is_rust_target { continue; }
            }
            if name == filename {
                results.push(path.to_string_lossy().to_string());
            }
            if path.is_dir() && depth < max_depth {
                queue.push_back((path, depth + 1));
            }
        }
    }
    results
}

pub(super) async fn find_csproj_in(dir: &str) -> Option<String> {
    if let Ok(mut rd) = tokio::fs::read_dir(dir).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if s.ends_with(".csproj") {
                return Some(entry.path().to_string_lossy().to_string());
            }
        }
    }
    None
}

/// Usa Nexus Gateway per raffinare `role` ed `essential` sui suggerimenti rilevati.
/// Se il gateway non è disponibile o la chiamata fallisce, le suggestions restano invariate.
pub(super) async fn refine_with_nexus(
    state: &AppState,
    project_id: Uuid,
    user_id: Uuid,
    root: &std::path::Path,
    suggestions: &mut Vec<Value>,
) {
    let gw = match &state.orchestrator.nexus_gateway {
        Some(g) => g,
        None => return,
    };

    // Costruisce il contesto: prime directory di primo livello + lista comandi
    let top_dirs: Vec<String> = std::fs::read_dir(root).ok()
        .map(|it| {
            let mut v: Vec<String> = it.filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| !n.starts_with('.') && n != "node_modules" && n != "target")
                .take(20)
                .collect();
            v.sort();
            v
        })
        .unwrap_or_default();

    let cmds: Vec<String> = suggestions.iter().enumerate().map(|(i, s)| {
        let label = s.get("label").and_then(|v| v.as_str()).unwrap_or("");
        let kind = s.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let cmd = s.get("command").and_then(|v| v.as_str()).unwrap_or("");
        let args: String = s.get("args").and_then(|a| a.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        let group = s.get("group").and_then(|v| v.as_str()).unwrap_or("");
        format!("{i}: [{kind}][{group}] {label}  →  {cmd} {args}")
    }).collect();

    let prompt = format!(
        "Sei un assistente per la classificazione di comandi di avvio applicazione.\n\
         Root progetto: {root}\n\
         Directory di primo livello: {top}\n\n\
         Classifica CIASCUN comando nell'elenco sottostante.\n\
         Rispondi ESCLUSIVAMENTE con un array JSON di esattamente {n} oggetti, \
         uno per riga, nel formato:\n\
         [{{\"role\":\"frontend\",\"essential\":true}}, ...]\n\n\
         Ruoli disponibili: frontend, backend, service, test, tool\n\
         essential = true se il processo deve girare per testare l'app end-to-end \
         (dev server, backend principale, docker-compose up), false altrimenti.\n\n\
         Comandi:\n{cmds}",
        root = root.display(),
        top = top_dirs.join(", "),
        n = suggestions.len(),
        cmds = cmds.join("\n"),
    );

    let req = GwRequest {
        model: "coder-small".to_string(),
        messages: vec![GwMessage { role: "user".to_string(), content: prompt }],
        max_tokens: Some(1024),
        temperature: Some(0.0),
        tools: None,
        metadata: GwMetadata {
            tenant_id: project_id.to_string(),
            user_id: user_id.to_string(),
            request_id: Uuid::new_v4().to_string(),
            sensitivity_tier: 0,
            feature: "detect_run_configs_ai".to_string(),
        },
    };

    let resp = match tokio::time::timeout(
        std::time::Duration::from_secs(15),
        gw.complete(req),
    ).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => { tracing::warn!("refine_with_nexus: gateway error: {e}"); return; }
        Err(_) => { tracing::warn!("refine_with_nexus: timeout"); return; }
    };

    // Tenta di estrarre il blocco JSON dall'eventuale testo libero
    let raw = resp.content.trim();
    let json_str = if raw.starts_with('[') {
        raw.to_string()
    } else if let (Some(s), Some(e)) = (raw.find('['), raw.rfind(']')) {
        raw[s..=e].to_string()
    } else {
        tracing::warn!("refine_with_nexus: risposta non parsabile: {raw}");
        return;
    };

    let parsed: Vec<serde_json::Value> = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(e) => { tracing::warn!("refine_with_nexus: JSON parse error: {e}"); return; }
    };

    for (i, item) in parsed.iter().enumerate() {
        if i >= suggestions.len() { break; }
        if let Some(role) = item.get("role").and_then(|v| v.as_str()) {
            suggestions[i]["role"] = json!(role);
        }
        if let Some(essential) = item.get("essential").and_then(|v| v.as_bool()) {
            suggestions[i]["essential"] = json!(essential);
        }
    }
}

/// Classifica il ruolo semantico di un comando di run.
pub(super) fn classify_role(kind: &str, name: &str, pkg: Option<&serde_json::Value>) -> &'static str {
    if kind == "playwright" { return "test"; }
    let lname = name.to_lowercase();
    if lname == "test" || lname.starts_with("test:") || lname == "cargo test"
        || lname == "go test ./..." || lname == "dotnet test"
    { return "test"; }

    let tool_prefixes = ["lint", "format", "fmt", "check", "typecheck", "tsc",
                         "build", "compile", "i18n", "ai:guard", "quality"];
    for t in &tool_prefixes {
        if lname == *t || lname.starts_with(&format!("{}:", t)) { return "tool"; }
    }
    if lname.starts_with("cargo build") { return "tool"; }

    if kind == "shell" && (lname.starts_with("docker") || lname == "docker-compose up") {
        return "service";
    }

    if kind == "npm" {
        if let Some(pkg) = pkg {
            let deps = pkg.get("dependencies").and_then(|v| v.as_object());
            let dev_deps = pkg.get("devDependencies").and_then(|v| v.as_object());
            let has_dep = |key: &str| -> bool {
                deps.map_or(false, |d| d.contains_key(key))
                    || dev_deps.map_or(false, |d| d.contains_key(key))
            };
            if has_dep("next") || has_dep("react") || has_dep("vite") || has_dep("vue")
                || has_dep("svelte") || has_dep("astro") || has_dep("@angular/core")
            { return "frontend"; }
            if let Some(pkg_name) = pkg.get("name").and_then(|v| v.as_str()) {
                let low = pkg_name.to_lowercase();
                if low.contains("api") || low.contains("server") || low.contains("backend")
                    || low.contains("gateway") || low.contains("service") || low.contains("worker")
                    || low.contains("brain") || low.contains("mcp")
                { return "backend"; }
            }
        }
        if matches!(lname.as_str(), "dev" | "start" | "serve" | "preview") { return "frontend"; }
        return "tool";
    }

    if kind == "cargo" || kind == "python" { return "backend"; }
    if kind == "shell" && (lname == "go run ." || lname == "dotnet run") { return "backend"; }

    "tool"
}

/// True se la configurazione è essenziale per avviare l'app end-to-end.
pub(super) fn is_essential(role: &str, name: &str, kind: &str) -> bool {
    match role {
        "frontend" | "backend" => matches!(name, "dev" | "start" | "serve")
            || kind == "cargo"
            || kind == "python"
            || name == "go run ."
            || name == "dotnet run",
        "service" => name == "docker-compose up" || name.starts_with("docker-compose up "),
        _ => false,
    }
}

/// Raccoglie directory dei workspace JS (package.json::workspaces + pnpm-workspace.yaml),
/// fallback a scan di `apps/*`, `packages/*`, `services/*`, `crates/*` e subdir dirette.
pub(super) fn collect_workspace_dirs(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = vec![root.to_path_buf()];
    let mut patterns: Vec<String> = Vec::new();

    if let Ok(content) = std::fs::read_to_string(root.join("package.json")) {
        if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(ws) = pkg.get("workspaces") {
                let arr = if ws.is_array() { ws.as_array() }
                          else { ws.get("packages").and_then(|p| p.as_array()) };
                if let Some(arr) = arr {
                    for v in arr {
                        if let Some(s) = v.as_str() { patterns.push(s.to_string()); }
                    }
                }
            }
        }
    }

    if let Ok(content) = std::fs::read_to_string(root.join("pnpm-workspace.yaml")) {
        let mut in_packages = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("packages:") { in_packages = true; continue; }
            if in_packages {
                if let Some(rest) = trimmed.strip_prefix("- ") {
                    let pat = rest.trim().trim_matches(|c| c == '"' || c == '\'');
                    patterns.push(pat.to_string());
                } else if !trimmed.is_empty() && !trimmed.starts_with('#')
                    && !trimmed.starts_with('-')
                {
                    in_packages = false;
                }
            }
        }
    }

    if patterns.is_empty() {
        for std_dir in &["apps", "packages", "services"] {
            patterns.push(format!("{}/*", std_dir));
        }
        patterns.push("*".to_string());
    }

    let skip = ["node_modules", "target", "dist", ".next", "build", "out"];
    for pat in &patterns {
        let (parent, is_glob) = if let Some(p) = pat.strip_suffix("/*") {
            (root.join(p), true)
        } else if pat == "*" {
            (root.to_path_buf(), true)
        } else {
            (root.join(pat), false)
        };
        if is_glob {
            if let Ok(entries) = std::fs::read_dir(&parent) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let p = entry.path();
                    if !p.is_dir() { continue; }
                    let n = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                    if n.starts_with('.') || skip.contains(&n.as_str()) { continue; }
                    if p.join("package.json").exists() && !dirs.contains(&p) {
                        dirs.push(p);
                    }
                }
            }
        } else if parent.join("package.json").exists() && !dirs.contains(&parent) {
            dirs.push(parent);
        }
    }
    dirs
}

/// Estrae i member paths di un Cargo workspace dal Cargo.toml root (supporta glob `crates/*`).
pub(super) fn collect_cargo_workspace_members(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut raw: Vec<String> = Vec::new();
    let content = match std::fs::read_to_string(root.join("Cargo.toml")) {
        Ok(c) => c, Err(_) => return Vec::new(),
    };
    let mut in_workspace = false;
    let mut in_members = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_workspace = trimmed == "[workspace]";
            in_members = false;
            continue;
        }
        if !in_workspace { continue; }
        if trimmed.starts_with("members") {
            in_members = true;
            if let Some(start) = trimmed.find('[') {
                let rest = &trimmed[start+1..];
                for tok in rest.split(',') {
                    let t = tok.trim().trim_matches(|c: char| c == '[' || c == ']' || c == '"' || c == '\'');
                    if !t.is_empty() { raw.push(t.to_string()); }
                }
                if trimmed.contains(']') { in_members = false; }
            }
            continue;
        }
        if in_members {
            if trimmed.contains(']') { in_members = false; continue; }
            let t = trimmed.trim_matches(|c: char| c == ',' || c == '"' || c == '\'');
            if !t.is_empty() { raw.push(t.to_string()); }
        }
    }

    let mut out = Vec::new();
    for m in raw {
        if let Some(prefix) = m.strip_suffix("/*") {
            let parent = root.join(prefix);
            if let Ok(entries) = std::fs::read_dir(&parent) {
                for e in entries.filter_map(|e| e.ok()) {
                    let p = e.path();
                    if p.is_dir() && p.join("Cargo.toml").exists() { out.push(p); }
                }
            }
        } else {
            let p = root.join(&m);
            if p.join("Cargo.toml").exists() { out.push(p); }
        }
    }
    out
}

/// Parser minimale di docker-compose: estrae i nomi dei service al primo livello di indentazione.
pub(crate) fn parse_compose_services(path: &std::path::Path) -> Vec<String> {
    let mut services = Vec::new();
    let content = match std::fs::read_to_string(path) { Ok(c) => c, Err(_) => return services };
    let mut in_services = false;
    let mut svc_indent: Option<usize> = None;
    for line in content.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') { continue; }
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim_end();
        let key = trimmed.trim_start();
        if !in_services {
            if key == "services:" { in_services = true; svc_indent = None; }
            continue;
        }
        if indent == 0 && key.ends_with(':') && key != "services:" { break; }
        if svc_indent.is_none() && indent > 0 { svc_indent = Some(indent); }
        if Some(indent) == svc_indent {
            if let Some(name) = key.strip_suffix(':') {
                if !name.is_empty() && !name.contains(' ') { services.push(name.to_string()); }
            }
        }
    }
    services
}

/// Raccoglie i file compose della root ordinati per priorità (dev, local, base, prod).
/// Matcha `docker-compose*.y(a)ml` e `compose*.y(a)ml`.
pub(crate) fn collect_compose_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else { return out; };
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_file() { continue; }
        let name = match p.file_name().map(|n| n.to_string_lossy().to_lowercase()) {
            Some(n) => n,
            None => continue,
        };
        let has_compose_prefix = name.starts_with("docker-compose") || name.starts_with("compose");
        let has_yaml_ext = name.ends_with(".yml") || name.ends_with(".yaml");
        if has_compose_prefix && has_yaml_ext {
            out.push(p);
        }
    }
    out.sort_by_key(|p| (compose_file_rank(p),
        p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    out
}

pub(super) fn detect_playwright_suggestions(root: &std::path::Path) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut pw_dirs: Vec<std::path::PathBuf> = Vec::new();
    for c in &["playwright.config.ts", "playwright.config.js", "playwright.config.mjs"] {
        if root.join(c).exists() { pw_dirs.push(root.to_path_buf()); break; }
    }
    if pw_dirs.is_empty() {
        if let Ok(entries) = std::fs::read_dir(root) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    for c in &["playwright.config.ts", "playwright.config.js"] {
                        if p.join(c).exists() { pw_dirs.push(p.clone()); break; }
                    }
                }
            }
        }
    }
    for pw_dir in &pw_dirs {
        let is_root = pw_dir == root;
        let cwd_val: Value = if is_root { Value::Null } else { json!(pw_dir.to_string_lossy()) };
        let pkg_manager = if pw_dir.join("pnpm-lock.yaml").exists() || root.join("pnpm-lock.yaml").exists() { "pnpm" }
            else if pw_dir.join("yarn.lock").exists() || root.join("yarn.lock").exists() { "yarn" }
            else { "npm" };
        let dir_label = if is_root { "root".to_string() }
            else { pw_dir.strip_prefix(root).ok()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|| pw_dir.file_name().unwrap_or_default().to_string_lossy().to_string()) };
        let prefix = if is_root { String::new() } else { format!("[{}] ", dir_label) };
        let group = format!("playwright/{}", dir_label);
        push_sugg(&mut out,
            format!("{}playwright test", prefix), "playwright", pkg_manager,
            vec![json!("exec"), json!("playwright"), json!("test")],
            cwd_val.clone(), json!({}), "test", false, group.clone());
        push_sugg(&mut out,
            format!("{}playwright test --update-snapshots", prefix), "playwright", pkg_manager,
            vec![json!("exec"), json!("playwright"), json!("test"), json!("--update-snapshots")],
            cwd_val.clone(), json!({}), "test", false, group.clone());
        for sub in &["tests", "e2e", "test"] {
            let tests_root = pw_dir.join(sub);
            if !tests_root.exists() { continue; }
            for spec in walkdir_specs(&tests_root).iter().take(10) {
                let rel = spec.strip_prefix(if is_root { root } else { pw_dir })
                    .unwrap_or(spec).to_string_lossy().replace('\\', "/");
                let name = spec.file_stem().unwrap_or_default().to_string_lossy()
                    .trim_end_matches(".spec").to_string();
                push_sugg(&mut out,
                    format!("{}playwright · {}", prefix, name), "playwright", pkg_manager,
                    vec![json!("exec"), json!("playwright"), json!("test"), json!(rel)],
                    cwd_val.clone(), json!({}), "test", false, group.clone());
            }
        }
    }
    out
}

/// 0 = dev, 1 = local, 2 = base (nessun suffisso), 3 = prod/altro.
pub(super) fn compose_file_rank(p: &std::path::Path) -> u8 {
    let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let stem = name.trim_end_matches(".yml").trim_end_matches(".yaml").to_string();
    // stem tipo: docker-compose, docker-compose.dev, compose.prod, ecc.
    let suffix = stem.rsplit('.').next().unwrap_or("");
    match suffix {
        "dev" | "development" => 0,
        "local" | "override" => 1,
        "prod" | "production" | "staging" | "ci" => 3,
        "" => 2,
        _ => {
            // Se il suffisso è l'intero stem → è il file base (es. "compose", "docker-compose").
            if suffix == stem { 2 } else { 3 }
        }
    }
}

/// Estrae il corpo (righe che iniziano con TAB) di un target Makefile fino alla prossima
/// riga non indentata. Ritorna stringa vuota se il target non è trovato.
pub(super) fn extract_make_target_body(content: &str, target: &str) -> String {
    let mut body = String::new();
    let mut in_target = false;
    let target_prefix = format!("{}:", target);
    for line in content.lines() {
        if !in_target {
            let trimmed = line.trim_start();
            if trimmed.starts_with(&target_prefix) {
                in_target = true;
            }
            continue;
        }
        if line.starts_with('\t') {
            body.push_str(line);
            body.push('\n');
        } else if line.trim().is_empty() || line.starts_with('#') {
            continue;
        } else {
            break;
        }
    }
    body
}

/// Helper condiviso da tutte le funzioni di detection run-config.
#[inline]
pub(super) fn push_sugg(
    out: &mut Vec<Value>, label: String, kind: &str, command: &str,
    args: Vec<Value>, cwd: Value, env: Value,
    role: &str, essential: bool, group: String,
) {
    out.push(json!({
        "label": label, "kind": kind, "command": command,
        "args": args, "cwd": cwd, "env": env,
        "role": role, "essential": essential, "group": group,
    }));
}

/// Cerca .sln fino a 2 livelli (root + primo livello di subdirectory).
/// Per ogni .sln emette `dotnet run --project <dir>` per i csproj Web/Exe e `dotnet test` per i test.
pub(super) fn detect_dotnet_suggestions(root: &std::path::Path) -> Vec<Value> {
    fn dir_has_sln(dir: &std::path::Path) -> bool {
        std::fs::read_dir(dir).ok()
            .map(|d| d.flatten().any(|e| e.path().extension().map(|x| x == "sln").unwrap_or(false)))
            .unwrap_or(false)
    }

    fn classify_csproj(path: &std::path::Path) -> Option<&'static str> {
        let content = std::fs::read_to_string(path).unwrap_or_default();
        let name_lc = path.file_stem().unwrap_or_default().to_string_lossy().to_lowercase();
        if name_lc.contains("test") || name_lc.contains("spec")
            || content.contains("xunit") || content.contains("nunit") || content.contains("MSTest") {
            return Some("test");
        }
        if content.contains("Sdk.Web") || content.contains("OutputType>Exe") {
            return Some("run");
        }
        None
    }

    let mut sln_dirs: Vec<(std::path::PathBuf, String)> = Vec::new();
    if dir_has_sln(root) {
        sln_dirs.push((root.to_path_buf(), String::new()));
    }
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() && dir_has_sln(&p) {
                let label = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
                sln_dirs.push((p, label));
            }
        }
    }

    // `dotnet run` richiede il .NET SDK installato sull'host, che non è disponibile
    // nel sandbox. Impostiamo sempre essential=false e aggiungiamo il suffisso al gruppo
    // così l'utente è informato prima di selezionare la configurazione.
    // Se il progetto è già containerizzato (Dockerfile/compose presente) il suffisso
    // esplicita ulteriormente che l'esecuzione host richiede SDK locale.
    let containerized = root.join("Dockerfile").exists()
        || root.join("Dockerfile.dev").exists()
        || !collect_compose_files(root).is_empty();
    let run_essential = false;
    let group_suffix = if containerized { " (host — richiede SDK locale)" } else { " (richiede .NET SDK)" };

    let mut out: Vec<Value> = Vec::new();
    for (sln_dir, dir_label) in &sln_dirs {
        let base_group = if dir_label.is_empty() { "dotnet".to_string() } else { dir_label.clone() };
        let group = format!("{}{}", base_group, group_suffix);
        let mut runnable: Vec<std::path::PathBuf> = Vec::new();
        let mut has_tests = false;

        if let Ok(entries) = std::fs::read_dir(sln_dir) {
            for e in entries.flatten() {
                let p = e.path();
                let search_dirs: Vec<std::path::PathBuf> = if p.is_dir() { vec![p] } else { vec![] };
                for dir in search_dirs {
                    if let Ok(inner) = std::fs::read_dir(&dir) {
                        for ie in inner.flatten() {
                            let ip = ie.path();
                            if ip.extension().map(|x| x == "csproj").unwrap_or(false) {
                                match classify_csproj(&ip) {
                                    Some("run") => runnable.push(ip),
                                    Some("test") => has_tests = true,
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                // anche .csproj direttamente nella sln_dir
                if e.path().extension().map(|x| x == "csproj").unwrap_or(false) {
                    match classify_csproj(&e.path()) {
                        Some("run") => runnable.push(e.path()),
                        Some("test") => has_tests = true,
                        _ => {}
                    }
                }
            }
        }

        let sdk_notice = " [richiede .NET SDK]";
        if runnable.is_empty() {
            let run_args: Vec<serde_json::Value> = if dir_label.is_empty() {
                vec![json!("run")]
            } else {
                vec![json!("run"), json!("--project"), json!(dir_label.clone())]
            };
            let cmd = if dir_label.is_empty() {
                format!("dotnet run{}", sdk_notice)
            } else {
                format!("dotnet run --project {}{}", dir_label, sdk_notice)
            };
            out.push(json!({ "label": cmd, "kind": "shell", "command": "dotnet",
                "args": run_args, "cwd": null, "env": {},
                "role": "backend", "essential": run_essential, "group": group }));
        } else {
            for csproj in &runnable {
                let rel = csproj.strip_prefix(root).unwrap_or(csproj);
                let proj_dir = rel.parent().map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                let cmd = if proj_dir.is_empty() {
                    format!("dotnet run{}", sdk_notice)
                } else {
                    format!("dotnet run --project {}{}", proj_dir, sdk_notice)
                };
                let run_args: Vec<serde_json::Value> = if proj_dir.is_empty() {
                    vec![json!("run")]
                } else {
                    vec![json!("run"), json!("--project"), json!(proj_dir.clone())]
                };
                out.push(json!({ "label": cmd, "kind": "shell", "command": "dotnet",
                    "args": run_args, "cwd": null, "env": {},
                    "role": "backend", "essential": run_essential, "group": group.clone() }));
            }
        }
        if has_tests {
            let test_cmd = if dir_label.is_empty() { "dotnet test".to_string() }
                           else { format!("dotnet test {}", dir_label) };
            let test_args: Vec<serde_json::Value> = if dir_label.is_empty() {
                vec![json!("test")]
            } else {
                vec![json!("test"), json!(dir_label.clone())]
            };
            out.push(json!({ "label": test_cmd, "kind": "shell", "command": "dotnet",
                "args": test_args, "cwd": null, "env": {},
                "role": "test", "essential": false, "group": group.clone() }));
        }
    }
    out
}
