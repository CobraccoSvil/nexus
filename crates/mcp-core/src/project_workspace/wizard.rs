use super::*;

// ── Auto-setup ambiente: rilevamento e installazione dipendenze ───────────────

/// Descrive un singolo step di setup dell'ambiente (es. `pnpm install`).
struct SetupStep {
    /// Nome leggibile dello step (es. "pnpm install")
    label: &'static str,
    /// Eseguibile da lanciare
    cmd: &'static str,
    /// Argomenti da passare all'eseguibile
    args: &'static [&'static str],
    /// File/directory che indica la presenza del progetto (es. "package.json")
    indicator: &'static str,
    /// File/directory che indica che il setup è GIA' stato fatto (es. "node_modules").
    /// Se questo path NON esiste, lo step viene eseguito.
    done_marker: &'static str,
}

/// Tabella di rilevamento framework → step di setup.
/// Ordinata per priorità: lock file prima del file generico dello stesso ecosistema.
static SETUP_STEPS: &[SetupStep] = &[
    // ── Node.js ──────────────────────────────────────────────────────────────
    SetupStep {
        label: "pnpm install",
        cmd: "pnpm",
        args: &["install", "--frozen-lockfile"],
        indicator: "pnpm-lock.yaml",
        done_marker: "node_modules",
    },
    SetupStep {
        label: "yarn install",
        cmd: "yarn",
        args: &["install", "--frozen-lockfile"],
        indicator: "yarn.lock",
        done_marker: "node_modules",
    },
    SetupStep {
        label: "npm install",
        cmd: "npm",
        args: &["install"],
        indicator: "package.json",
        done_marker: "node_modules",
    },
    // ── .NET ─────────────────────────────────────────────────────────────────
    // Nota: l'indicatore *.csproj viene gestito a parte (glob) perché ha
    // un'estensione variabile; qui usiamo il file di soluzione come proxy.
    SetupStep {
        label: "dotnet restore",
        cmd: "dotnet",
        args: &["restore"],
        indicator: "*.csproj",
        done_marker: "bin",
    },
    // ── Python ───────────────────────────────────────────────────────────────
    SetupStep {
        label: "uv sync",
        cmd: "uv",
        args: &["sync"],
        indicator: "uv.lock",
        done_marker: ".venv",
    },
    SetupStep {
        label: "poetry install",
        cmd: "poetry",
        args: &["install", "--no-interaction"],
        indicator: "poetry.lock",
        done_marker: ".venv",
    },
    SetupStep {
        label: "pip install",
        cmd: "pip",
        args: &["install", "-r", "requirements.txt"],
        indicator: "requirements.txt",
        done_marker: ".venv",
    },
    SetupStep {
        label: "pipenv install",
        cmd: "pipenv",
        args: &["install"],
        indicator: "Pipfile",
        done_marker: ".venv",
    },
    // ── Go ───────────────────────────────────────────────────────────────────
    SetupStep {
        label: "go mod download",
        cmd: "go",
        args: &["mod", "download"],
        indicator: "go.mod",
        done_marker: "vendor",
    },
    // ── Ruby ─────────────────────────────────────────────────────────────────
    SetupStep {
        label: "bundle install",
        cmd: "bundle",
        args: &["install"],
        indicator: "Gemfile",
        done_marker: "vendor/bundle",
    },
    // ── PHP ───────────────────────────────────────────────────────────────────
    SetupStep {
        label: "composer install",
        cmd: "composer",
        args: &["install", "--no-interaction"],
        indicator: "composer.json",
        done_marker: "vendor",
    },
];

/// Rileva quale step di setup è necessario per il `cwd` dato e lo esegue.
/// Restituisce la lista degli step eseguiti con il relativo esito.
/// Il `done_marker` per `*.csproj` viene gestito tramite glob; per tutti gli
/// altri framework il match è diretto sul nome del file.
async fn run_env_setup(cwd: &str, unit_name: &str) -> Vec<serde_json::Value> {
    let mut log: Vec<serde_json::Value> = Vec::new();

    for step in SETUP_STEPS {
        // Controlla se l'indicatore esiste nella directory
        let indicator_exists = if step.indicator.contains('*') {
            // Glob semplice: cerca file con quell'estensione nella directory
            let ext = step.indicator.trim_start_matches('*');
            tokio::fs::read_dir(cwd)
                .await
                .ok()
                .map(|rd| {
                    // La lettura in async richiede un loop; usiamo std come fallback
                    // perché il read_dir async non ha un metodo .any() diretto.
                    drop(rd);
                    std::fs::read_dir(cwd)
                        .ok()
                        .map(|rd| {
                            rd.filter_map(|e| e.ok())
                                .any(|e| e.file_name().to_string_lossy().ends_with(ext))
                        })
                        .unwrap_or(false)
                })
                .unwrap_or(false)
        } else {
            tokio::fs::metadata(format!("{}/{}", cwd, step.indicator))
                .await
                .is_ok()
        };

        if !indicator_exists {
            continue;
        }

        // Controlla se il done_marker è già presente (setup già fatto)
        let done = tokio::fs::metadata(format!("{}/{}", cwd, step.done_marker))
            .await
            .is_ok();
        if done {
            tracing::debug!(unit = %unit_name, cwd = %cwd, step = %step.label, "setup già presente, skip");
            continue;
        }

        // Esegui lo step
        tracing::info!(unit = %unit_name, cwd = %cwd, step = %step.label, "eseguo setup ambiente");
        let result = tokio::process::Command::new(step.cmd)
            .args(step.args)
            .current_dir(cwd)
            .output()
            .await;

        match result {
            Ok(out) => {
                let ok = out.status.success();
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                // Ultime 15 righe per non saturare la risposta JSON
                let tail = |s: &str| {
                    s.lines()
                        .rev()
                        .take(15)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                if ok {
                    tracing::info!(unit = %unit_name, cwd = %cwd, step = %step.label, "setup completato");
                } else {
                    tracing::warn!(
                        unit = %unit_name, cwd = %cwd, step = %step.label,
                        stderr = %stderr, "setup fallito (exit {:?})", out.status.code()
                    );
                }
                log.push(json!({
                    "step":   step.label,
                    "ok":     ok,
                    "stdout": tail(&stdout),
                    "stderr": if ok { "".to_string() } else { tail(&stderr) },
                }));
            }
            Err(e) => {
                tracing::warn!(unit = %unit_name, cwd = %cwd, step = %step.label,
                               "impossibile eseguire setup: {}", e);
                log.push(json!({ "step": step.label, "ok": false, "error": e.to_string() }));
            }
        }

        // Un solo step per directory: il primo match vince.
        // (es. se c'è pnpm-lock.yaml non eseguiamo anche npm install)
        break;
    }

    log
}

// ── Helper systemd user ───────────────────────────────────────────────────────

/// Risolve `XDG_RUNTIME_DIR` per il processo corrente.
/// Se la variabile d'ambiente è già impostata (es. sessione interattiva), la usa.
/// Altrimenti legge l'UID effettivo da `/proc/self/status` e costruisce il path.
/// Questo è necessario quando `mcp-core` gira come servizio system senza variabili
/// di sessione utente nell'ambiente (es. `systemctl start nexus-core.service`).
fn resolve_xdg_runtime_dir() -> String {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return dir;
    }
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        if let Some(line) = status.lines().find(|l| l.starts_with("Uid:")) {
            if let Some(uid) = line.split_whitespace().nth(1) {
                return format!("/run/user/{}", uid);
            }
        }
    }
    "/run/user/1000".to_string()
}

/// Crea un `Command` per `systemctl` pre-configurato con le variabili d'ambiente
/// necessarie per operare in modalità `--user` anche da un servizio system.
fn systemctl_user() -> tokio::process::Command {
    let xdg = resolve_xdg_runtime_dir();
    let bus = format!("unix:path={}/bus", xdg);
    let mut cmd = tokio::process::Command::new("systemctl");
    cmd.env("XDG_RUNTIME_DIR", xdg)
        .env("DBUS_SESSION_BUS_ADDRESS", bus);
    cmd
}

/// Marcatori che indicano l'assenza del manager `systemd --user`.
/// Tipicamente in WSL o in container senza `user@UID.service` attivo.
fn systemd_bus_unavailable(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("failed to connect to bus")
        || t.contains("connection refused")
        || t.contains("no such file or directory")
        || t.contains("host is down")
}

/// Verifica se il manager `systemd --user` risponde.
///
/// Esegue `systemctl --user is-system-running`: in un ambiente con user manager
/// attivo ritorna `running`/`degraded`/`starting` (anche con exit code != 0 nel
/// caso `degraded`, ma il bus risponde). In WSL senza user manager il comando
/// stampa "Failed to connect to bus: Connection refused" su stderr.
///
/// Default conservativo: in caso di dubbio (errore di spawn, output ambiguo)
/// ritorna `false` -> si va in fallback detached, che e' sempre sicuro.
pub(crate) async fn systemd_user_available() -> bool {
    match systemctl_user()
        .args(["--user", "is-system-running"])
        .output()
        .await
    {
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            if systemd_bus_unavailable(&stderr) || systemd_bus_unavailable(&stdout) {
                return false;
            }
            // Il manager risponde: stdout contiene uno stato noto.
            let s = stdout.trim();
            matches!(
                s,
                "running" | "degraded" | "starting" | "initializing" | "stopping" | "maintenance"
            )
        }
        Err(_) => false,
    }
}

/// Avvia il comando del servizio come processo detached persistente, scollegato
/// dal ciclo di vita di mcp-core. Usato come fallback quando `systemd --user`
/// non e' disponibile (es. WSL senza user manager).
///
/// Pattern collaudato nel repo (vedi `task_watchdog::try_restart_gateway`):
/// `setsid nohup ... > LOGFILE 2>&1 < /dev/null &`. `setsid` crea una nuova
/// sessione: il figlio non riceve SIGTERM/SIGHUP quando mcp-core termina.
///
/// Idempotenza best-effort: prima di avviare, termina un eventuale processo
/// detached precedente che esegua lo stesso `exec_start` (match su pattern).
///
/// Ritorna `Ok(logfile)` in caso di spawn riuscito, `Err(msg)` se lo spawn
/// stesso fallisce (in tal caso il chiamante deve ritornare ok:false).
pub(super) async fn spawn_detached_service(
    unit_name: &str,
    cwd: &str,
    env_map: &std::collections::HashMap<String, String>,
    exec_start: &str,
) -> Result<String, String> {
    let logfile = format!("/tmp/nexus-proj-{}.log", unit_name);

    // Idempotenza best-effort: termina un eventuale detached precedente che
    // gira lo stesso comando. pkill -f match sull'exec_start (non bloccante).
    let _ = tokio::process::Command::new("pkill")
        .args(["-f", exec_start])
        .output()
        .await;

    // Stringa env in stile `KEY='val' KEY2='val2'` (quoting singolo, escape ').
    let env_str: String = env_map
        .iter()
        .map(|(k, v)| format!("{}='{}' ", k, v.replace('\'', "'\\''")))
        .collect();

    // setsid nohup env <ENV> bash -lc 'exec <CMD>' > LOG 2>&1 < /dev/null &
    let shell = format!(
        "cd '{}' && setsid nohup env {}bash -lc 'exec {}' > '{}' 2>&1 < /dev/null &",
        cwd.replace('\'', "'\\''"),
        env_str,
        exec_start.replace('\'', "'\\''"),
        logfile,
    );

    match tokio::process::Command::new("/bin/sh")
        .args(["-c", &shell])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
    {
        Ok(out) if out.status.success() => Ok(logfile),
        Ok(out) => Err(format!(
            "spawn detached fallito: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => Err(format!("spawn detached: shell exec fallito: {}", e)),
    }
}

// ── Wizard ────────────────────────────────────────────────────────────────────

/// Analizza il filesystem del progetto e suggerisce definizioni di servizi systemd.
/// Riconosce: npm/pnpm scripts, Cargo binaries, .csproj / launchSettings.json,
/// docker-compose.yml, python app entry points.
/// Comandi no-op che genererebbero un servizio segnaposto (sempre `active
/// (exited)` ma inutile). PUNTO UNICO (regola L): usato sia da
/// `wizard_install_service` sia da `service_discovery::validate_and_map_service`.
pub(super) const FORBIDDEN_NOOP: &[&str] = &[
    "true", "false", ":", "sleep", "echo", "exit", "noop", "no-op",
];

/// Marca come `existing=true` i suggerimenti gia' gestiti dal progetto. PUNTO
/// UNICO (regola L): usato sia dal ramo agentico sia dall'euristica in
/// `wizard_detect_services`. Considera DUE fonti, cosi' la dialog del wizard
/// (che decide "Installato vs Installa" su questo flag) resta allineata al badge
/// e alla lista dei servizi gestiti:
///   1. unit registrate in systemd --user (`list-unit-files`), quando il bus c'e';
///   2. file unit presenti su disco (`services::project_unit_files_on_disk`), che
///      e' l'unica fonte disponibile in WSL/detached dove il bus utente e' giu' e
///      il punto 1 ritorna vuoto. E' la stessa fonte di `list_services_fallback`.
/// Stato volatile: non va cachato.
pub(super) async fn mark_existing_services(slug: &str, suggestions: &mut [serde_json::Value]) {
    let mut installed: std::collections::HashSet<String> = std::collections::HashSet::new();
    // 1. Unit registrate in systemd --user (quando il bus risponde).
    if let Ok(svc_out) = systemctl_user()
        .args([
            "--user",
            "list-unit-files",
            "--type=service",
            "--no-legend",
            "--no-pager",
        ])
        .output()
        .await
    {
        installed.extend(
            String::from_utf8_lossy(&svc_out.stdout)
                .lines()
                .filter_map(|l| l.split_whitespace().next().map(String::from)),
        );
    }
    // 2. Unit presenti come FILE su disco: in WSL/detached il bus e' giu' (punto 1
    //    vuoto) ma i file unit esistono e il pannello li mostra gia' come gestiti.
    installed.extend(super::services::project_unit_files_on_disk(slug).await);
    for s in suggestions.iter_mut() {
        let unit = s["unit"].as_str().unwrap_or("");
        if installed.contains(unit) {
            s["existing"] = json!(true);
        }
    }
}

// safety: pattern literal valido
static RE_COMPOSE_PORT_VAR: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\$\{(PORT[A-Z0-9_]*)(?::-\d+)?\}").unwrap()
});

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

    // Porta deterministica per suggerimenti web: evita conflitti già in fase di analisi.
    async fn suggest_port(state: &AppState, project_id: &Uuid, key: &str) -> u16 {
        super::services::deterministic_project_port_for_key(project_id, key, &state.port_registry)
            .await
    }

    // ── Rilevamento agentico PRIMARIO (agent-first) ────────────────────────
    // Se l'agent identifica servizi validi, li usa e ritorna subito; altrimenti
    // prosegue con l'euristica testuale sottostante come fallback (regola: agent
    // primario, euristica rete di sicurezza). L'agent fa solo la comprensione;
    // l'allocazione porte resta deterministica dentro service_discovery.
    if let Some(mut found) = super::service_discovery::discover_services_agentic(
        &state,
        &project_id,
        &root,
        &context.details.name,
        &slug,
    )
    .await
    {
        if !found.is_empty() {
            mark_existing_services(&slug, &mut found).await;
            super::service_discovery::drop_managed_variants(&mut found).await;
            return Ok(Json(json!({ "suggestions": found, "slug": slug })));
        }
    }

    // ── 1. package.json / pnpm ─────────────────────────────────────────────
    let pkg_paths = find_files_named(&root, "package.json", 6).await;
    for pkg_path in &pkg_paths {
        if let Ok(content) = tokio::fs::read_to_string(pkg_path).await {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&content) {
                let scripts = pkg.get("scripts").and_then(|s| s.as_object());
                let cwd = std::path::Path::new(pkg_path)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| root.clone());
                let rel = cwd
                    .strip_prefix(&root)
                    .unwrap_or("")
                    .trim_start_matches('/');
                let pkg_manager = if tokio::fs::metadata(format!("{}/pnpm-lock.yaml", cwd))
                    .await
                    .is_ok()
                {
                    "pnpm"
                } else {
                    "npm"
                };
                // Controlla se node_modules esiste. Il flag `needs_install`
                // viene usato dalla UI per mostrare uno step "Setup ambiente";
                // il wizard install eseguirà automaticamente il setup.
                let needs_install = tokio::fs::metadata(format!("{}/node_modules", &cwd))
                    .await
                    .is_err();
                for script_name in ["dev", "start", "serve", "preview"] {
                    if scripts
                        .map(|s| s.contains_key(script_name))
                        .unwrap_or(false)
                    {
                        let svc_short = if rel.is_empty() {
                            script_name.to_string()
                        } else {
                            format!("{}-{}", rel.replace('/', "-"), script_name)
                        };
                        suggestions.push(json!({
                            "short":         svc_short,
                            "unit":          format!("{}-{}.service", slug, svc_short),
                            "label":         format!("{} {} ({})", pkg_manager, script_name, if rel.is_empty() { "root" } else { rel }),
                            "kind":          if pkg_manager == "pnpm" { "pnpm" } else { "npm" },
                            "command":       pkg_manager,
                            "args":          ["run", script_name],
                            "cwd":           cwd,
                            "env":           { "PORT": suggest_port(&state, &project_id, &svc_short).await.to_string() },
                            "existing":      false,
                            "needs_install": needs_install,
                            "pkg_manager":   pkg_manager,
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
        let cwd = std::path::Path::new(lp)
            .parent()
            .and_then(|p| p.parent()) // Properties/ → project dir
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| root.clone());
        let csproj = find_csproj_in(&cwd).await;
        let proj_arg = csproj.as_deref().unwrap_or(".");
        let rel = cwd
            .strip_prefix(&root)
            .unwrap_or("")
            .trim_start_matches('/');
        let svc_short = if rel.is_empty() {
            "dotnet".to_string()
        } else {
            rel.replace('/', "-")
        };
        // needs_install: manca la directory bin/ → dotnet restore necessario
        let needs_install = tokio::fs::metadata(format!("{}/bin", cwd)).await.is_err();
        suggestions.push(json!({
            "short":         svc_short,
            "unit":          format!("{}-{}.service", slug, svc_short),
            "label":         format!("dotnet run ({})", if rel.is_empty() { "root" } else { rel }),
            "kind":          "dotnet",
            "command":       "dotnet",
            "args":          ["run", "--project", proj_arg],
            "cwd":           cwd,
            "env":           { "PORT": suggest_port(&state, &project_id, &svc_short).await.to_string() },
            "existing":      false,
            "needs_install": needs_install,
            "pkg_manager":   "dotnet restore",
        }));
    }

    // ── 3. Cargo.toml binaries ─────────────────────────────────────────────
    let cargo_paths = find_files_named(&root, "Cargo.toml", 6).await;
    for cp in &cargo_paths {
        if let Ok(content) = tokio::fs::read_to_string(cp).await {
            // Cerca [[bin]] entries
            let bin_names: Vec<String> = content
                .lines()
                .filter_map(|l| {
                    let t = l.trim();
                    if t.starts_with("name") && content.contains("[[bin]]") {
                        t.split_once('=').map(|x| x.1)
                            .map(|v| v.trim().trim_matches('"').to_string())
                    } else {
                        None
                    }
                })
                .collect();
            let cwd = std::path::Path::new(cp)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| root.clone());
            let rel = cwd
                .strip_prefix(&root)
                .unwrap_or("")
                .trim_start_matches('/');
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
    for dc_name in &[
        "docker-compose.yml",
        "docker-compose.yaml",
        "docker-compose.dev.yml",
        "docker-compose.dev.yaml",
    ] {
        let dc_path = format!("{}/{}", root, dc_name);
        if tokio::fs::metadata(&dc_path).await.is_ok() {
            // docker-compose espone tipicamente PIU' porte via variabili
            // d'ambiente (es. ${PORT_FRONTEND:-20001}, ${PORT_BACKEND:-20002}).
            // Allochiamo una porta gestita per CIASCUNA variabile e la
            // proponiamo nell'env, invece di lasciare il dialog vuoto (che
            // mostra il placeholder fuorviante "PORT=20000") o iniettare una
            // singola PORT che il compose ignorerebbe. Cosi' il servizio docker
            // usa le porte del bucket di progetto (regola I / ADR 0010).
            let mut env_obj = serde_json::Map::new();
            if let Ok(compose) = tokio::fs::read_to_string(&dc_path).await {
                let mut seen = std::collections::HashSet::new();
                for cap in RE_COMPOSE_PORT_VAR.captures_iter(&compose) {
                    if let Some(m) = cap.get(1) {
                        let var = m.as_str().to_string();
                        if seen.insert(var.clone()) {
                            let key = format!("docker-{}", var.to_lowercase());
                            let p = suggest_port(&state, &project_id, &key).await;
                            env_obj.insert(var, serde_json::Value::String(p.to_string()));
                        }
                    }
                }
            }
            suggestions.push(json!({
                "short":    "docker",
                "unit":     format!("{}-docker.service", slug),
                "label":    format!("docker compose up ({})", dc_name),
                "kind":     "shell",
                "command":  "docker",
                "args":     ["compose", "-f", dc_name, "up"],
                "cwd":      root,
                "existing": false,
                "env":      serde_json::Value::Object(env_obj),
            }));
            break;
        }
    }

    // ── 5. Python entry points ─────────────────────────────────────────────
    for py_entry in &["main.py", "app.py", "server.py", "run.py", "manage.py"] {
        let py_path = format!("{}/{}", root, py_entry);
        if tokio::fs::metadata(&py_path).await.is_ok() {
            let svc_short = py_entry.strip_suffix(".py").unwrap_or(py_entry);
            // Rileva il package manager Python e se l'ambiente è già pronto
            let (py_pkg_manager, needs_install) = {
                let venv_ok = tokio::fs::metadata(format!("{}/.venv", root)).await.is_ok()
                    || tokio::fs::metadata(format!("{}/venv", root)).await.is_ok();
                let pm = if tokio::fs::metadata(format!("{}/uv.lock", root))
                    .await
                    .is_ok()
                {
                    "uv sync"
                } else if tokio::fs::metadata(format!("{}/poetry.lock", root))
                    .await
                    .is_ok()
                {
                    "poetry install"
                } else if tokio::fs::metadata(format!("{}/Pipfile", root))
                    .await
                    .is_ok()
                {
                    "pipenv install"
                } else if tokio::fs::metadata(format!("{}/requirements.txt", root))
                    .await
                    .is_ok()
                {
                    "pip install -r requirements.txt"
                } else {
                    ""
                };
                (pm, !venv_ok && !pm.is_empty())
            };
            suggestions.push(json!({
                "short":         svc_short,
                "unit":          format!("{}-{}.service", slug, svc_short),
                "label":         format!("python {} (root)", py_entry),
                "kind":          "python",
                "command":       "python3",
                "args":          [py_entry],
                "cwd":           root,
                "existing":      false,
                "needs_install": needs_install,
                "pkg_manager":   py_pkg_manager,
            }));
        }
    }

    // ── 6. Sito HTML statico (index.html o primo .html) senza framework ────
    // Punto unico (regola L): stessa rilevazione del server integrato
    // (static_preview::detect_static_entry). Proposto SOLO se nessun framework
    // serve gia' il sito (package.json/Cargo.toml/launchSettings/python entry).
    // Il server e' un python http.server su una PORTA ALLOCATA DA NEXUS
    // (kind="static" -> wants_port nell'install): mai porte riservate (la 8080
    // e' in NEXUS_RESERVED_PORTS) ne' hardcoded. La porta concreta e' iniettata
    // dall'install al posto del placeholder __PORT__.
    {
        let mut has_framework = tokio::fs::metadata(format!("{}/package.json", root))
            .await
            .is_ok()
            || tokio::fs::metadata(format!("{}/Cargo.toml", root))
                .await
                .is_ok()
            || tokio::fs::metadata(format!("{}/launchSettings.json", root))
                .await
                .is_ok();
        if !has_framework {
            for f in &["main.py", "app.py", "server.py", "run.py", "manage.py"] {
                if tokio::fs::metadata(format!("{}/{}", root, f)).await.is_ok() {
                    has_framework = true;
                    break;
                }
            }
        }
        if !has_framework {
            if let Some(entry) = crate::static_preview::detect_static_entry(&root).await {
                suggestions.push(json!({
                    "short":         "static",
                    "unit":          format!("{}-static.service", slug),
                    "label":         format!("Server statico HTML ({entry})"),
                    "kind":          "static",
                    "command":       "python3",
                    "args":          ["-m", "http.server", "__PORT__", "--bind", "127.0.0.1"],
                    "cwd":           root,
                    "existing":      false,
                    "needs_install": false,
                }));
            }
        }
    }

    // Marca quelli già installati come .service files (punto unico, regola L).
    mark_existing_services(&slug, &mut suggestions).await;
    // Scarta le varianti di avvio di servizi gia' gestiti (punto unico, regola L).
    super::service_discovery::drop_managed_variants(&mut suggestions).await;

    Ok(Json(json!({ "suggestions": suggestions, "slug": slug })))
}

/// Per i servizi docker-compose scrive/aggiorna il file `.env` nella directory
/// del compose (`cwd`) con le variabili di porta allocate (`PORT*`). Cosi' anche
/// un `docker compose up` lanciato A MANO (non via unit systemd) usa le porte del
/// bucket gestito invece dei default hardcoded nel compose (es.
/// `${PORT_FRONTEND:-20001}`), che facevano ripartire i container su porte fuori
/// bucket. Merge NON distruttivo: aggiorna le righe PORT* esistenti, aggiunge le
/// mancanti, preserva tutto il resto. (regola I: porte sempre dal registro.)
async fn write_compose_env_file(cwd: &str, env_map: &std::collections::HashMap<String, String>) {
    let ports: std::collections::BTreeMap<&str, &str> = env_map
        .iter()
        .filter(|(k, _)| k.starts_with("PORT"))
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    if ports.is_empty() {
        return;
    }
    let env_path = format!("{}/.env", cwd.trim_end_matches('/'));
    let existing = tokio::fs::read_to_string(&env_path)
        .await
        .unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    let mut written: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in existing.lines() {
        let key = line.split('=').next().unwrap_or("").trim();
        if let Some(v) = ports.get(key) {
            out.push(format!("{}={}", key, v));
            written.insert(key.to_string());
        } else {
            out.push(line.to_string());
        }
    }
    for (k, v) in &ports {
        if !written.contains(*k) {
            out.push(format!("{}={}", k, v));
        }
    }
    let content = format!("{}\n", out.join("\n"));
    match tokio::fs::write(&env_path, &content).await {
        Ok(()) => tracing::info!(
            "docker-compose: .env aggiornato con le porte gestite del bucket ({})",
            env_path
        ),
        Err(e) => tracing::warn!("docker-compose: scrittura .env fallita ({}): {e}", env_path),
    }
}

/// Genera (e scrive) l'override docker-compose con il mapping porte coerente:
/// host = porta del bucket di progetto, container = porta interna reale del
/// servizio. Ritorna il nome del file override da passare a `docker compose -f`,
/// oppure None se non c'e' nulla da rimappare. La logica di parsing/render e' nel
/// punto unico `compose_ports` (regola L); qui si alloca la porta host dal
/// registro e si scrive il file. Risolve il mismatch host/container (es. vite su
/// 20001 dentro il container vs porta host gestita) SENZA toccare i file del
/// progetto.
async fn generate_docker_port_override(
    state: &AppState,
    project_id: &Uuid,
    cwd: &str,
) -> Option<(String, String)> {
    use super::compose_ports;
    let dir = cwd.trim_end_matches('/');
    let base_name = [
        "docker-compose.yml",
        "docker-compose.yaml",
        "docker-compose.dev.yml",
        "docker-compose.dev.yaml",
        "compose.yml",
        "compose.yaml",
    ]
    .iter()
    .copied()
    .find(|n| std::path::Path::new(&format!("{dir}/{n}")).exists())?;
    let base = format!("{dir}/{base_name}");
    let content = tokio::fs::read_to_string(&base).await.ok()?;
    let plans = compose_ports::planned_mappings(&compose_ports::parse_service_ports(&content));
    if plans.is_empty() {
        return None;
    }
    let mut entries: Vec<compose_ports::OverrideEntry> = Vec::new();
    for pm in plans {
        let key = format!("docker-{}-{}", pm.service, pm.container);
        let host = super::services::deterministic_project_port_for_key(
            project_id,
            &key,
            &state.port_registry,
        )
        .await;
        if let Some(e) = entries.iter_mut().find(|e| e.service == pm.service) {
            e.ports.push(format!("{host}:{}", pm.container));
            for v in pm.vars {
                if !e.env.iter().any(|(k, _)| *k == v) {
                    e.env.push((v, pm.container.to_string()));
                }
            }
        } else {
            let env: Vec<(String, String)> = pm
                .vars
                .into_iter()
                .map(|v| (v, pm.container.to_string()))
                .collect();
            entries.push(compose_ports::OverrideEntry {
                service: pm.service.clone(),
                ports: vec![format!("{host}:{}", pm.container)],
                env,
            });
        }
    }
    let yaml = compose_ports::render_override_yaml(&entries);
    let path = format!("{dir}/{}", compose_ports::OVERRIDE_FILE);
    match tokio::fs::write(&path, yaml).await {
        Ok(()) => {
            tracing::info!(
                "docker-compose: override porte coerente generato ({path}) sopra {base_name}"
            );
            Some((
                base_name.to_string(),
                compose_ports::OVERRIDE_FILE.to_string(),
            ))
        }
        Err(e) => {
            tracing::warn!("docker-compose: scrittura override fallita ({path}): {e}");
            None
        }
    }
}

/// Inserisce `-f <base> -f <override>` nell'ExecStart di un `docker compose ...
/// up`, subito prima del sottocomando `up`. Entrambi i flag servono perche'
/// `docker compose` DISATTIVA il discovery automatico del compose base quando
/// vede un `-f` esplicito: passare solo `-f override` farebbe perdere il file
/// base e tutti i suoi servizi. Se l'ExecStart contiene gia' qualunque `-f`
/// (es. l'utente ha specificato un compose custom) non si tocca il base, si
/// aggiunge SOLO l'override per non sovrascrivere la scelta. Idempotente.
fn inject_override_flag(exec_start: &str, base_file: &str, override_file: &str) -> String {
    if exec_start.contains(override_file) {
        // L'override e' gia' presente. Idempotente SOLO se accompagnato da almeno
        // un altro `-f` (il base, qualunque esso sia): l'override Nexus e' parziale
        // (solo i mapping ports), quindi da solo NON basta — mancherebbero
        // image/build/volumes dei servizi. Se l'override e' l'UNICO `-f` (caso
        // reale osservato su Beauty-Book: ExecStart `docker compose -f
        // docker-compose.nexus.yml up`), anteponiamo il base prima del primo `-f`
        // preservando l'ordine base-poi-override.
        let f_count = exec_start.matches("-f ").count();
        if f_count >= 2 {
            return exec_start.to_string();
        }
        if let Some(pos) = exec_start.find("-f ") {
            let (head, tail) = exec_start.split_at(pos);
            return format!("{head}-f {base_file} {tail}");
        }
        return exec_start.to_string();
    }
    let has_explicit_f = exec_start.contains(" -f ") || exec_start.contains(" --file ");
    let flags = if has_explicit_f {
        format!("-f {override_file} ")
    } else {
        format!("-f {base_file} -f {override_file} ")
    };
    if let Some(pos) = exec_start.find(" up") {
        let (head, tail) = exec_start.split_at(pos + 1);
        format!("{head}{flags}{tail}")
    } else {
        format!("{exec_start} {flags}").trim_end().to_string()
    }
}

/// Deriva le env var di un frontend dai servizi sibling gia' allocati al
/// progetto (punto unico, regola L): `BACKEND_API_URL` e `VITE_API_URL` dalla
/// porta del backend sibling, `NEXTAUTH_URL` dalla porta del frontend stesso.
/// Unica fonte di verita' = le allocazioni in `nexus_port_allocations` (regola
/// G): la porta del backend non viene mai hardcoded. Inerte se il servizio non
/// e' un frontend o se non c'e' un backend sibling. Le guard `!contains_key`
/// non sovrascrivono valori espliciti dell'utente. Funzione pura (nessun I/O)
/// per essere testabile: l'incidente login Beauty-Book (proxy /api verso porta
/// vuota -> HTTP 500) nasceva dal fatto che `VITE_API_URL` non era generata.
pub(crate) fn derive_frontend_sibling_env(
    env_map: &mut std::collections::HashMap<String, String>,
    sibling_ports: &[(i32, String)],
    exec_start: &str,
    kind: &str,
) {
    let exec_lower = exec_start.to_lowercase();
    let is_frontend = matches!(kind, "npm" | "pnpm")
        || exec_lower.contains("next")
        || exec_lower.contains("vite")
        || exec_lower.contains("react-scripts")
        || exec_lower.contains("nuxt")
        || exec_lower.contains("astro");
    if !is_frontend {
        return;
    }
    // BACKEND_API_URL / VITE_API_URL: porta sibling con label "backend-*" o "api-*"
    let backend_sibling = sibling_ports.iter().find(|item| {
        let l = item.1.to_lowercase();
        l.starts_with("backend") || l.starts_with("api-") || l.starts_with("api_")
    });
    if let Some((port, _)) = backend_sibling {
        let backend_url = format!("http://127.0.0.1:{}", port);
        if !env_map.contains_key("BACKEND_API_URL")
            && !env_map.contains_key("BACKEND_API_INTERNAL_URL")
        {
            env_map.insert("BACKEND_API_URL".to_string(), backend_url.clone());
        }
        // VITE_API_URL: i frontend Vite leggono import.meta.env.VITE_API_URL (e il
        // proxy di vite.config la usa come target /api) per raggiungere il backend.
        // Inerte per i frontend non-Vite (non la leggono).
        if !env_map.contains_key("VITE_API_URL") {
            env_map.insert("VITE_API_URL".to_string(), backend_url);
        }
    }
    // NEXTAUTH_URL: per Next.js, se non gia' impostata esplicitamente
    let wants_nextauth = exec_lower.contains("next")
        || (exec_lower.contains("npm") && exec_lower.contains("start"))
        || (exec_lower.contains("pnpm") && exec_lower.contains("start"));
    if wants_nextauth && !env_map.contains_key("NEXTAUTH_URL") {
        if let Some(p) = env_map.get("PORT").and_then(|s| s.parse::<u16>().ok()) {
            env_map.insert("NEXTAUTH_URL".to_string(), format!("http://localhost:{}", p));
        }
    }
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

    let short = body["short"]
        .as_str()
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Campo 'short' obbligatorio"))?;
    let command = body["command"]
        .as_str()
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "Campo 'command' obbligatorio"))?;
    let root_str = context.root_path.to_string_lossy().to_string();
    let cwd = body["cwd"].as_str().unwrap_or(&root_str);
    let desc_fallback = format!("{} {}", context.details.name, short);
    let description = body["description"].as_str().unwrap_or(&desc_fallback);

    if short.contains('/') || short.contains("..") {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Nome servizio non valido",
        ));
    }

    // ── Validazione anti-placeholder ─────────────────────────────────────
    // Rifiuta comandi vuoti o no-op come `/bin/true`, `/bin/false`, `:`, `true`,
    // `false`, `sleep`, ecc. — generavano servizi "fantasma" che apparivano
    // sempre `active (exited)` ma non facevano nulla, confondendo l'utente.
    let cmd_trim = command.trim();
    if cmd_trim.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il comando del servizio non può essere vuoto",
        ));
    }
    let cmd_basename = std::path::Path::new(cmd_trim)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(cmd_trim);
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
            format!(
                "La directory di lavoro '{}' non esiste o non è accessibile: {}",
                cwd, e
            ),
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
    let args: Vec<String> = body["args"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
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
                .output()
                .await;
            match r {
                Ok(out) if out.status.success() => {
                    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if p.is_empty() {
                        None
                    } else {
                        Some(p)
                    }
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
        if t.is_empty() {
            return None;
        }
        let t = t.trim_matches(|c: char| c == '"' || c == '\'' || c == ',' || c == ';');
        t.parse::<u16>().ok()
    }

    fn looks_like_web_server_command(command: &str) -> bool {
        // Deve funzionare anche quando `command` è un path assoluto (es. /usr/bin/npm)
        // e quando l'argomento contiene più token.
        let lower = command.to_lowercase();
        // Match basati su word-boundary per evitare dipendenze dalla presenza di spazi iniziali.
        let has = |pat: &str| lower.contains(pat);
        has("next dev")
            || has("next start")
            || has("react-scripts start")
            || has("vite")
            || has("nuxt")
            || has("astro")
            || (has("pnpm") && has("run") && has(" dev"))
            || (has("npm") && has("run") && has(" dev"))
            || (has("npm") && has(" start"))
            || (has("yarn") && has(" dev"))
            || (has("dotnet") && has(" run"))
    }

    /// Risolve quale tool reale esegue uno script npm/pnpm/yarn leggendo
    /// `package.json` nella `cwd`. Es: `pnpm run dev` → "vite" se
    /// `scripts.dev = "vite"`. Ritorna `Some(tool_command)` se trovato.
    /// Importante per Vite/Astro/Nuxt che IGNORANO $PORT env e richiedono
    /// `--port` esplicito sulla command line.
    fn resolve_script_command(cwd: &str, exec: &str) -> Option<String> {
        let lower = exec.to_lowercase();
        let script_name = if lower.contains(" run dev") || lower.ends_with(" dev") {
            "dev"
        } else if lower.contains(" run start") || lower.contains(" start") {
            "start"
        } else if lower.contains(" run serve") {
            "serve"
        } else {
            return None;
        };
        let pkg = std::path::Path::new(cwd).join("package.json");
        let content = std::fs::read_to_string(&pkg).ok()?;
        let parsed: serde_json::Value = serde_json::from_str(&content).ok()?;
        let cmd = parsed
            .get("scripts")?
            .get(script_name)?
            .as_str()?
            .to_string();
        if cmd.trim().is_empty() {
            None
        } else {
            Some(cmd)
        }
    }

    fn rewrite_port_flags(command: &str, target_port: u16) -> String {
        let p = target_port.to_string();
        let mut out = command.to_string();
        // RAFFORZAMENTO guard-rail porte (ADR 0010 / regola I): riscrive QUALUNQUE
        // porta esplicita sui flag --port/-p con la porta ALLOCATA dal registro,
        // non solo i default noti dei framework. Senza questo, una porta hardcoded
        // ARBITRARIA nel bucket (es. `vite --port 20001` mai passata da
        // request_port) sopravvive: il servizio ascolta su una porta diversa da
        // Environment=PORT / dall'allocazione -> mismatch e kill del port-enforcer
        // (caso reale Beauty-Book: ExecStart `--port 20001` con PORT=39598).
        // Cattura: `--port N`, `--port=N`, ` -p N`, ` -p=N`, ` -pN`. Gruppo 1 =
        // flag+separatore (preservato), gruppo 2 = numero (sostituito).
        match regex::Regex::new(r"(--port[ =]|(?:^|\s)-p[ =]?)(\d{2,5})") {
            Ok(re) => {
                out = re
                    .replace_all(&out, |c: &regex::Captures| format!("{}{}", &c[1], p))
                    .into_owned();
            }
            Err(_) => {
                // Ramo difensivo (regex statica valida: mai raggiunto in pratica).
                for bad in [
                    "3000", "5173", "4321", "3001", "8080", "4200", "5000", "8000",
                ] {
                    out = out.replace(&format!("--port {}", bad), &format!("--port {}", p));
                    out = out.replace(&format!("--port={}", bad), &format!("--port={}", p));
                }
            }
        }
        // URL espliciti verso i default noti -> porta allocata. Qui resta una
        // blacklist mirata (non una regex generica) per NON riscrivere porte
        // legittime in URL verso servizi esterni o DB (es. 5432, 6379).
        for bad in [
            "3000", "3001", "3002", "4200", "4321", "5173", "5174", "5000", "5001", "8000", "8080",
            "9000",
        ] {
            out = out.replace(&format!("localhost:{}", bad), &format!("localhost:{}", p));
            out = out.replace(&format!("127.0.0.1:{}", bad), &format!("127.0.0.1:{}", p));
        }
        let lower = out.to_lowercase();
        let has_flag = lower.contains("--port")
            || lower
                .split_whitespace()
                .any(|t| t == "-p" || t.starts_with("-p"));
        // Forza --port per tool che ignorano $PORT env var:
        // - Vite (5173 default, ignora PORT senza --port o vite.config)
        // - Astro (4321 default, idem)
        // - Nuxt (3000 default, idem)
        // - Next.js (3000 default, accetta -p)
        // - Svelte/Kit (5173 default tramite Vite)
        if !has_flag {
            let needs_vite_port = lower.contains("vite") || lower.contains("svelte");
            let needs_astro_port = lower.contains("astro");
            let needs_nuxt_port = lower.contains("nuxt");
            let needs_next_port = lower.contains("next dev") || lower.contains("next start");
            if needs_vite_port || needs_astro_port || needs_nuxt_port {
                out.push_str(&format!(" --port {}", p));
            } else if needs_next_port {
                out.push_str(&format!(" -p {}", p));
            }
        }
        out
    }

    // Blocco Environment= per variabili d'ambiente (con policy porte: mai usare porte riservate Nexus, incl. 3000).
    let reserved: std::collections::HashSet<u16> =
        services::NEXUS_RESERVED_PORTS.iter().copied().collect();
    let mut env_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if let Some(obj) = body["env"].as_object() {
        for (k, v) in obj {
            env_map.insert(k.clone(), v.as_str().unwrap_or("").to_string());
        }
    }

    let kind = body["kind"].as_str().unwrap_or("");
    let wants_port = matches!(kind, "npm" | "pnpm" | "dotnet" | "static")
        || looks_like_web_server_command(&exec_start);
    let existing_port = env_map.get("PORT").and_then(|v| parse_port_token(v));
    let final_port = if wants_port {
        let actual = match existing_port {
            // Il client/utente ha forzato PORT: rispettalo, ma validalo e ripiega su
            // una porta libera del bucket se occupata/riservata.
            Some(p) => {
                let ok = !reserved.contains(&p)
                    && state.port_registry.is_port_available(p).await
                    && tokio::net::TcpListener::bind(format!("127.0.0.1:{}", p))
                        .await
                        .is_ok();
                if ok {
                    p
                } else {
                    services::find_free_project_port(&project_id, &state.port_registry).await
                }
            }
            // Auto-allocazione: PUNTO UNICO (regola L). find_or_allocate riusa la
            // porta gia' persistita in nexus_port_allocations per (project_id, short)
            // se esiste, altrimenti ne alloca una deterministica nel bucket e la
            // PERSISTE. Cosi' un re-install/riavvio riprende SEMPRE la stessa porta
            // (A3: niente swap frontend<->backend) e l'allocazione non resta 0-righe
            // (causa radice login Beauty-Book: A2 non aveva una riga backend da cui
            // derivare VITE_API_URL). Niente piu' calcolo hash order-dependent qui.
            None => {
                match super::allocate_port::find_or_allocate(
                    &state.db,
                    &state.port_registry,
                    project_id,
                    short,
                )
                .await
                {
                    Ok(a) => a.port,
                    Err(e) => {
                        tracing::warn!(
                            "wizard: find_or_allocate fallita per {} ({}), fallback find_free",
                            short,
                            e
                        );
                        services::find_free_project_port(&project_id, &state.port_registry).await
                    }
                }
            }
        };
        env_map.insert("PORT".to_string(), actual.to_string());
        // .NET: usa ASPNETCORE_URLS per forzare la porta (PORT da solo non basta).
        if kind == "dotnet" && !env_map.contains_key("ASPNETCORE_URLS") {
            env_map.insert(
                "ASPNETCORE_URLS".to_string(),
                format!("http://0.0.0.0:{}", actual),
            );
        }
        Some(actual)
    } else {
        None
    };

    let exec_start = if let Some(p) = final_port {
        // Per script alias (npm/pnpm/yarn run dev) prova a risolvere il tool
        // reale dal package.json. Necessario per Vite/Astro/Nuxt che ignorano
        // $PORT env e richiedono --port sulla CLI del tool, non del wrapper.
        let lower_exec = exec_start.to_lowercase();
        let is_script_runner = (lower_exec.contains("npm")
            || lower_exec.contains("pnpm")
            || lower_exec.contains("yarn"))
            && (lower_exec.contains(" run ")
                || lower_exec.ends_with(" dev")
                || lower_exec.ends_with(" start"));
        if is_script_runner {
            if let Some(resolved) = resolve_script_command(cwd, &exec_start) {
                let resolved_lower = resolved.to_lowercase();
                let uses_vite_like = resolved_lower.contains("vite")
                    || resolved_lower.contains("astro")
                    || resolved_lower.contains("nuxt")
                    || resolved_lower.contains("svelte");
                if uses_vite_like {
                    // Estrai il package manager (npm/pnpm/yarn) per usare `<pm> exec`.
                    let pm = if lower_exec.contains("pnpm") {
                        "pnpm exec"
                    } else if lower_exec.contains("yarn") {
                        "yarn"
                    } else {
                        "npx"
                    };
                    // Aggiungi --host solo se lo script risolto non lo ha gia',
                    // altrimenti l'ExecStart finisce con `--host 0.0.0.0` duplicato.
                    let needs_host = (resolved_lower.contains("vite")
                        || resolved_lower.contains("svelte"))
                        && !resolved_lower.contains("--host");
                    let host_flag = if needs_host { " --host 0.0.0.0" } else { "" };
                    // resolved e' qualcosa come "vite" o "vite --some-flag" — manteniamo i suoi flag
                    // ma aggiungiamo --port se manca.
                    let resolved_rewritten = rewrite_port_flags(&resolved, p);
                    let needs_port_append = !resolved_rewritten.to_lowercase().contains("--port")
                        && !resolved_rewritten.to_lowercase().contains(" -p ");
                    let port_flag = if needs_port_append {
                        format!(" --port {}", p)
                    } else {
                        String::new()
                    };
                    format!("{} {}{}{}", pm, resolved_rewritten, port_flag, host_flag)
                } else {
                    rewrite_port_flags(&exec_start, p)
                }
            } else {
                rewrite_port_flags(&exec_start, p)
            }
        } else {
            rewrite_port_flags(&exec_start, p)
        }
    } else {
        exec_start
    };

    // Inietta la porta allocata da Nexus al posto del placeholder dei servizi
    // statici (kind="static": `python -m http.server __PORT__`). La porta e'
    // scritta NUMERICA nell'unit e nel fallback detached: robusta, niente
    // espansione shell (un `${PORT}` finirebbe dentro gli apici singoli del
    // fallback `bash -lc 'exec ...'` e non verrebbe espanso).
    let exec_start = match final_port {
        Some(p) => exec_start.replace("__PORT__", &p.to_string()),
        None => exec_start,
    };

    // ── Variabili cross-servizio ──────────────────────────────────────────────
    // Inietta automaticamente l'URL del backend/frontend tra i servizi sibling
    // già allocati al progetto. Evita porte hardcoded nei file .service.
    // Si esegue dopo l'assegnazione di PORT/ASPNETCORE_URLS per includere la
    // porta appena allocata nelle ricerche sibling.
    {
        #[derive(sqlx::FromRow)]
        struct PortLabel {
            port: i32,
            label: String,
        }
        let sibling_rows: Vec<PortLabel> = sqlx::query_as(
            "SELECT port, label FROM nexus_port_allocations \
             WHERE project_id = $1 AND label != $2 ORDER BY port ASC",
        )
        .bind(project_id)
        .bind(short)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        let sibling_ports: Vec<(i32, String)> = sibling_rows
            .into_iter()
            .map(|r| (r.port, r.label))
            .collect();

        // Derivazione env frontend dai sibling (punto unico, regola L): vedi
        // derive_frontend_sibling_env. exec_start qui e' ancora l'originale (il
        // rebind per l'override docker avviene piu' sotto).
        derive_frontend_sibling_env(&mut env_map, &sibling_ports, &exec_start, kind);
    }

    // Funzione unica (regola L): per i servizi docker-compose, Nexus genera un
    // OVERRIDE deterministico (docker-compose.nexus.yml) con il mapping coerente
    // host_bucket:porta_interna. Risolve il mismatch (es. vite --port 20001
    // hardcoded dentro il container vs porta host gestita): l'override fissa il
    // mapping reale + la variabile di porta -> il servizio risponde sulla porta
    // host gestita SENZA toccare i file del progetto.
    let exec_start = {
        let el = exec_start.to_lowercase();
        if el.contains("docker compose") || el.contains("docker-compose") {
            if let Some((base, ov)) = generate_docker_port_override(&state, &project_id, cwd).await
            {
                inject_override_flag(&exec_start, &base, &ov)
            } else {
                exec_start
            }
        } else {
            exec_start
        }
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

    // ── Coerenza modalita' di avvio del progetto (regola L, run_mode) ─────────
    // Un progetto usa UNA sola modalita': NATIVO (npm/dotnet/...) oppure CONTAINER
    // (docker compose). Installare un servizio nell'altra modalita' crea unit che
    // si uccidono a vicenda (SIGTERM) -> il sito non parte (incidente beauty-book:
    // backend.service npm + docker-compose insieme). Difesa autoritativa nel punto
    // unico di creazione unit: si rifiuta il mix con 409, indicando come switchare.
    // Fail-open: se la dir non e' leggibile o non si classifica, si procede come
    // prima (nessuna regressione sul flusso esistente).
    {
        use super::run_mode::{exec_start_of_unit, run_mode_of};
        let candidate_mode = run_mode_of(&exec_start);
        if let Ok(mut rd) = tokio::fs::read_dir(&svc_dir).await {
            let prefix = format!("{}-", slug);
            while let Ok(Some(ent)) = rd.next_entry().await {
                let fname = ent.file_name().to_string_lossy().to_string();
                if fname == unit_name || !fname.starts_with(&prefix) || !fname.ends_with(".service")
                {
                    continue;
                }
                let Ok(content) = tokio::fs::read_to_string(ent.path()).await else {
                    continue;
                };
                if let Some(es) = exec_start_of_unit(&content) {
                    let other_mode = run_mode_of(&es);
                    if other_mode != candidate_mode {
                        return Err(api_error(
                            StatusCode::CONFLICT,
                            format!(
                                "Conflitto di modalita' di avvio: il progetto ha gia' il servizio \
                                 '{}' in modalita' {}, mentre '{}' sarebbe {}. Avvio nativo e \
                                 container per lo stesso progetto si uccidono a vicenda (SIGTERM) e \
                                 il sito non parte. Disinstalla i servizi dell'altra modalita' \
                                 prima di installare questo, oppure usa la stessa modalita'.",
                                fname,
                                other_mode.label(),
                                unit_name,
                                candidate_mode.label()
                            ),
                        ));
                    }
                }
            }
        }
    }

    tokio::fs::create_dir_all(&svc_dir)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("mkdir: {}", e)))?;
    tokio::fs::write(&svc_path, &unit_content)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("write: {}", e)))?;

    // Cleanup: rimuove servizi disabled dello stesso progetto con ruolo sovrapponibile.
    // Es. se stiamo installando "backend-FreeLance.Api", rimuove "backend" (disabled)
    // perche' il nome corto del vecchio servizio e' un prefisso del nuovo.
    let mut cleaned: Vec<String> = Vec::new();
    let slug_prefix = format!("{}-", slug);
    if let Ok(list_out) = systemctl_user()
        .args([
            "--user",
            "list-unit-files",
            "--type=service",
            "--no-legend",
            "--no-pager",
        ])
        .output()
        .await
    {
        for line in String::from_utf8_lossy(&list_out.stdout).lines() {
            let cols: Vec<&str> = line.split_whitespace().collect();
            let old_unit = cols.first().copied().unwrap_or("");
            let old_state = cols.get(1).copied().unwrap_or("");
            if old_state != "disabled" {
                continue;
            }
            if !old_unit.starts_with(&slug_prefix) || !old_unit.ends_with(".service") {
                continue;
            }
            if old_unit == unit_name {
                continue;
            }
            let old_short = old_unit
                .strip_prefix(&slug_prefix)
                .unwrap_or(old_unit)
                .strip_suffix(".service")
                .unwrap_or(old_unit);
            if short.starts_with(old_short) || old_short.starts_with(short) {
                let old_path = format!("{}/{}", svc_dir, old_unit);
                let _ = systemctl_user()
                    .args(["--user", "stop", old_unit])
                    .output()
                    .await;
                let _ = systemctl_user()
                    .args(["--user", "disable", old_unit])
                    .output()
                    .await;
                let _ = tokio::fs::remove_file(&old_path).await;
                cleaned.push(old_unit.to_string());
                tracing::info!(
                    "Rimosso servizio orfano {} (sostituito da {})",
                    old_unit,
                    unit_name
                );
            }
        }
    }

    // Registra le porte del servizio nel port_registry (mig 0114).
    // Estrae le porte dal contenuto unit appena scritto e le registra come "auto".
    // Indipendente dalla strategia di avvio (systemd o detached).
    //
    // A3: la porta PRINCIPALE (final_port) e' gia' stata scelta e PERSISTITA da
    // find_or_allocate sopra, sulla riga (project_id, short). Qui basta valorizzarne
    // il service_unit (find_or_allocate lo lascia NULL) con un UPDATE idempotente, e
    // registrare le eventuali porte SECONDARIE estratte dalla unit (raro). Saltiamo
    // final_port nel loop per non collidere su uq_port_alloc_project_label.
    if let Some(fp) = final_port {
        if let Err(e) = sqlx::query(
            "UPDATE nexus_port_allocations SET service_unit = $1, updated_at = NOW() \
             WHERE project_id = $2 AND label = $3",
        )
        .bind(&unit_name)
        .bind(project_id)
        .bind(short)
        .execute(&state.db)
        .await
        {
            tracing::warn!(
                "wizard: update service_unit per {} (porta {}) fallito: {}",
                unit_name,
                fp,
                e
            );
        }
    }
    let detected_ports = services::extract_ports_from_unit(&unit_content);
    for p in &detected_ports {
        if Some(*p) == final_port {
            continue; // gia' persistita da find_or_allocate + service_unit sopra
        }
        // Ignora errori di registrazione (es. porta gia' allocata) — non blocca l'install
        if let Err(e) = state
            .port_registry
            .allocate(project_id, *p, short, "auto", None, Some(&unit_name))
            .await
        {
            tracing::warn!(
                "port_registry: registrazione porta {} per {} fallita: {}",
                p,
                unit_name,
                e
            );
        }
    }

    // Fix sistemico: per i servizi docker-compose scrivi/aggiorna il .env nella
    // dir del compose con le porte allocate. Cosi' anche un `docker compose up`
    // lanciato a mano (non via unit) usa le porte del bucket, non i default
    // hardcoded nel compose (es. ${PORT_FRONTEND:-20001}) che facevano ripartire
    // i container su porte fuori bucket ("ripresenta sempre 20001/20002").
    {
        let el = exec_start.to_lowercase();
        if el.contains("docker compose") || el.contains("docker-compose") {
            write_compose_env_file(cwd, &env_map).await;
        }
    }

    // ── Strategia di avvio ──────────────────────────────────────────────────
    // Se il manager `systemd --user` risponde: daemon-reload + enable --now (avvio
    // reale, persistente al riavvio). Altrimenti (es. WSL senza user manager):
    // fallback detached. In entrambi i casi la unit file e' gia' stata scritta,
    // utile per quando systemd --user diventera' disponibile.
    let ok: bool;
    let mode: &str;
    let mut warning: Option<String> = None;

    if systemd_user_available().await {
        mode = "systemd";
        let _ = systemctl_user()
            .args(["--user", "daemon-reload"])
            .output()
            .await;
        // enable --now: abilita all'avvio E avvia subito (prima si faceva solo
        // `enable`, quindi il servizio non partiva).
        let enable_out = systemctl_user()
            .args(["--user", "enable", "--now", &unit_name])
            .output()
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        ok = enable_out.status.success();
        if !ok {
            let stderr = String::from_utf8_lossy(&enable_out.stderr);
            let stdout = String::from_utf8_lossy(&enable_out.stdout);
            tracing::warn!(
                unit = %unit_name,
                stderr = %stderr,
                stdout = %stdout,
                "systemctl --user enable --now fallito"
            );
        }
    } else {
        mode = "detached_fallback";
        tracing::info!(
            unit = %unit_name,
            "systemd --user non disponibile: avvio servizio in modalita' detached"
        );
        match spawn_detached_service(&unit_name, cwd, &env_map, &exec_start).await {
            Ok(logfile) => {
                ok = true;
                warning = Some(format!(
                    "systemd --user non e' attivo in questo ambiente (es. WSL senza user manager): \
                     il servizio e' stato avviato in modalita' diretta (non persistente al riavvio \
                     del sistema). La unit e' stata salvata in ~/.config/systemd/user/ per quando \
                     systemd --user sara' disponibile. Log: {}",
                    logfile
                ));
            }
            Err(e) => {
                // Lo spawn detached stesso e' fallito: errore reale, ok:false.
                ok = false;
                warning = Some(format!("Avvio detached fallito: {}", e));
                tracing::warn!(unit = %unit_name, error = %e, "spawn detached fallito");
            }
        }
    }

    // ── Auto-setup ambiente: installa dipendenze per qualsiasi framework ────────
    // Rilevamento automatico: pnpm/yarn/npm, .NET, Python (uv/poetry/pip/pipenv),
    // Go, Ruby, PHP. Lo step viene saltato se il done_marker è già presente.
    let setup_log = run_env_setup(cwd, &unit_name).await;

    Ok(Json(json!({
        "ok":        ok,
        "unit":      unit_name,
        "path":      svc_path,
        "content":   unit_content,
        "cleaned":   cleaned,
        "setup_log": setup_log,
        "mode":      mode,
        "warning":   warning,
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
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Nome servizio non valido",
        ));
    }
    let unit_name = if service.starts_with(&format!("{}-", slug)) {
        if service.ends_with(".service") {
            service.clone()
        } else {
            format!("{}.service", service)
        }
    } else {
        format!("{}-{}.service", slug, service)
    };
    // Sicurezza ridondante
    if !unit_name.starts_with(&format!("{}-", slug)) {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "L'unit non appartiene al progetto",
        ));
    }

    // 1. stop (ignora errori: il servizio potrebbe già essere fermo)
    let _ = systemctl_user()
        .args(["--user", "stop", &unit_name])
        .output()
        .await;
    // 2. disable
    let _ = systemctl_user()
        .args(["--user", "disable", &unit_name])
        .output()
        .await;

    // 3. Prima di rimuovere il file, leggi il contenuto per estrarre le porte da rilasciare
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let svc_path = format!("{}/.config/systemd/user/{}", home, unit_name);

    // Rilascia porte dal port_registry leggendo il file prima di cancellarlo
    if let Ok(content) = tokio::fs::read_to_string(&svc_path).await {
        let ports = services::extract_ports_from_unit(&content);
        for p in ports {
            if let Err(e) = state.port_registry.release(p).await {
                tracing::debug!(
                    "port_registry: rilascio porta {} per {} ignorato: {}",
                    p,
                    unit_name,
                    e
                );
            }
        }
    }

    // 4. Rimozione del file unit
    let removed = match tokio::fs::remove_file(&svc_path).await {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            return Err(api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Impossibile rimuovere {}: {}", svc_path, e),
            ))
        }
    };

    // 5. daemon-reload
    let _ = systemctl_user()
        .args(["--user", "daemon-reload"])
        .output()
        .await;

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
        ".git",
        "node_modules",
        ".next",
        ".turbo",
        ".cache",
        "__pycache__",
        ".venv",
        "venv",
        "env",
        "obj",
        "bin", // .NET build output
        ".terraform",
        ".gradle", // build tools
        "vendor",  // Go/PHP vendor
    ];
    // Salta "target" solo se contiene a sua volta "debug" o "release" (indice di build Rust)
    let mut results = Vec::new();
    let mut queue: std::collections::VecDeque<(std::path::PathBuf, usize)> =
        std::collections::VecDeque::new();
    queue.push_back((std::path::PathBuf::from(root), 0));

    while let Some((dir, depth)) = queue.pop_front() {
        let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if SKIP.contains(&name) {
                continue;
            }
            // Salta "target/" solo se sembra una build Rust (ha "debug" o "release" al suo interno)
            if name == "target" && path.is_dir() {
                let is_rust_target = tokio::fs::metadata(path.join("debug")).await.is_ok()
                    || tokio::fs::metadata(path.join("release")).await.is_ok();
                if is_rust_target {
                    continue;
                }
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
    suggestions: &mut [Value],
) {
    let gw = match &state.orchestrator.nexus_gateway {
        Some(g) => g,
        None => return,
    };

    // Costruisce il contesto: prime directory di primo livello + lista comandi
    let top_dirs: Vec<String> = std::fs::read_dir(root)
        .ok()
        .map(|it| {
            let mut v: Vec<String> = it
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| !n.starts_with('.') && n != "node_modules" && n != "target")
                .take(20)
                .collect();
            v.sort();
            v
        })
        .unwrap_or_default();

    let cmds: Vec<String> = suggestions
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let label = s.get("label").and_then(|v| v.as_str()).unwrap_or("");
            let kind = s.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            let cmd = s.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let args: String = s
                .get("args")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let group = s.get("group").and_then(|v| v.as_str()).unwrap_or("");
            format!("{i}: [{kind}][{group}] {label}  →  {cmd} {args}")
        })
        .collect();

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
        messages: vec![GwMessage {
            role: "user".to_string(),
            content: serde_json::Value::String(prompt),
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
        }],
        max_tokens: Some(1024),
        temperature: Some(0.0),
        metadata: GwMetadata {
            tenant_id: project_id.to_string(),
            user_id: user_id.to_string(),
            request_id: Uuid::new_v4().to_string(),
            sensitivity_tier: 0,
            feature: "detect_run_configs_ai".to_string(),
        },
        ..Default::default()
    };

    let resp =
        match tokio::time::timeout(std::time::Duration::from_secs(15), gw.complete(req)).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tracing::warn!("refine_with_nexus: gateway error: {e}");
                return;
            }
            Err(_) => {
                tracing::warn!("refine_with_nexus: timeout");
                return;
            }
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
        Err(e) => {
            tracing::warn!("refine_with_nexus: JSON parse error: {e}");
            return;
        }
    };

    for (i, item) in parsed.iter().enumerate() {
        if i >= suggestions.len() {
            break;
        }
        if let Some(role) = item.get("role").and_then(|v| v.as_str()) {
            suggestions[i]["role"] = json!(role);
        }
        if let Some(essential) = item.get("essential").and_then(|v| v.as_bool()) {
            suggestions[i]["essential"] = json!(essential);
        }
    }
}

/// Classifica il ruolo semantico di un comando di run.
pub(super) fn classify_role(
    kind: &str,
    name: &str,
    pkg: Option<&serde_json::Value>,
) -> &'static str {
    if kind == "playwright" {
        return "test";
    }
    let lname = name.to_lowercase();
    if lname == "test"
        || lname.starts_with("test:")
        || lname == "cargo test"
        || lname == "go test ./..."
        || lname == "dotnet test"
    {
        return "test";
    }

    let tool_prefixes = [
        "lint",
        "format",
        "fmt",
        "check",
        "typecheck",
        "tsc",
        "build",
        "compile",
        "i18n",
        "ai:guard",
        "quality",
    ];
    for t in &tool_prefixes {
        if lname == *t || lname.starts_with(&format!("{}:", t)) {
            return "tool";
        }
    }
    if lname.starts_with("cargo build") {
        return "tool";
    }

    if kind == "shell" && (lname.starts_with("docker") || lname == "docker-compose up") {
        return "service";
    }

    if kind == "npm" {
        if let Some(pkg) = pkg {
            let deps = pkg.get("dependencies").and_then(|v| v.as_object());
            let dev_deps = pkg.get("devDependencies").and_then(|v| v.as_object());
            let has_dep = |key: &str| -> bool {
                deps.is_some_and(|d| d.contains_key(key))
                    || dev_deps.is_some_and(|d| d.contains_key(key))
            };
            if has_dep("next")
                || has_dep("react")
                || has_dep("vite")
                || has_dep("vue")
                || has_dep("svelte")
                || has_dep("astro")
                || has_dep("@angular/core")
            {
                return "frontend";
            }
            if let Some(pkg_name) = pkg.get("name").and_then(|v| v.as_str()) {
                let low = pkg_name.to_lowercase();
                if low.contains("api")
                    || low.contains("server")
                    || low.contains("backend")
                    || low.contains("gateway")
                    || low.contains("service")
                    || low.contains("worker")
                    || low.contains("brain")
                    || low.contains("mcp")
                {
                    return "backend";
                }
            }
        }
        if matches!(lname.as_str(), "dev" | "start" | "serve" | "preview") {
            return "frontend";
        }
        return "tool";
    }

    if kind == "cargo" || kind == "python" {
        return "backend";
    }
    if kind == "shell" && (lname == "go run ." || lname == "dotnet run") {
        return "backend";
    }

    "tool"
}

/// True se la configurazione è essenziale per avviare l'app end-to-end.
pub(super) fn is_essential(role: &str, name: &str, kind: &str) -> bool {
    match role {
        "frontend" | "backend" => {
            matches!(name, "dev" | "start" | "serve")
                || kind == "cargo"
                || kind == "python"
                || name == "go run ."
                || name == "dotnet run"
        }
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
                let arr = if ws.is_array() {
                    ws.as_array()
                } else {
                    ws.get("packages").and_then(|p| p.as_array())
                };
                if let Some(arr) = arr {
                    for v in arr {
                        if let Some(s) = v.as_str() {
                            patterns.push(s.to_string());
                        }
                    }
                }
            }
        }
    }

    if let Ok(content) = std::fs::read_to_string(root.join("pnpm-workspace.yaml")) {
        let mut in_packages = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("packages:") {
                in_packages = true;
                continue;
            }
            if in_packages {
                if let Some(rest) = trimmed.strip_prefix("- ") {
                    let pat = rest.trim().trim_matches(|c| c == '"' || c == '\'');
                    patterns.push(pat.to_string());
                } else if !trimmed.is_empty()
                    && !trimmed.starts_with('#')
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
                    if !p.is_dir() {
                        continue;
                    }
                    let n = p
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    if n.starts_with('.') || skip.contains(&n.as_str()) {
                        continue;
                    }
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
        Ok(c) => c,
        Err(_) => return Vec::new(),
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
        if !in_workspace {
            continue;
        }
        if trimmed.starts_with("members") {
            in_members = true;
            if let Some(start) = trimmed.find('[') {
                let rest = &trimmed[start + 1..];
                for tok in rest.split(',') {
                    let t = tok
                        .trim()
                        .trim_matches(|c: char| c == '[' || c == ']' || c == '"' || c == '\'');
                    if !t.is_empty() {
                        raw.push(t.to_string());
                    }
                }
                if trimmed.contains(']') {
                    in_members = false;
                }
            }
            continue;
        }
        if in_members {
            if trimmed.contains(']') {
                in_members = false;
                continue;
            }
            let t = trimmed.trim_matches(|c: char| c == ',' || c == '"' || c == '\'');
            if !t.is_empty() {
                raw.push(t.to_string());
            }
        }
    }

    let mut out = Vec::new();
    for m in raw {
        if let Some(prefix) = m.strip_suffix("/*") {
            let parent = root.join(prefix);
            if let Ok(entries) = std::fs::read_dir(&parent) {
                for e in entries.filter_map(|e| e.ok()) {
                    let p = e.path();
                    if p.is_dir() && p.join("Cargo.toml").exists() {
                        out.push(p);
                    }
                }
            }
        } else {
            let p = root.join(&m);
            if p.join("Cargo.toml").exists() {
                out.push(p);
            }
        }
    }
    out
}

/// Parser minimale di docker-compose: estrae i nomi dei service al primo livello di indentazione.
pub(crate) fn parse_compose_services(path: &std::path::Path) -> Vec<String> {
    let mut services = Vec::new();
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return services,
    };
    let mut in_services = false;
    let mut svc_indent: Option<usize> = None;
    for line in content.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim_end();
        let key = trimmed.trim_start();
        if !in_services {
            if key == "services:" {
                in_services = true;
                svc_indent = None;
            }
            continue;
        }
        if indent == 0 && key.ends_with(':') && key != "services:" {
            break;
        }
        if svc_indent.is_none() && indent > 0 {
            svc_indent = Some(indent);
        }
        if Some(indent) == svc_indent {
            if let Some(name) = key.strip_suffix(':') {
                if !name.is_empty() && !name.contains(' ') {
                    services.push(name.to_string());
                }
            }
        }
    }
    services
}

/// Raccoglie i file compose della root ordinati per priorità (dev, local, base, prod).
/// Matcha `docker-compose*.y(a)ml` e `compose*.y(a)ml`.
pub(crate) fn collect_compose_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
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
    out.sort_by_key(|p| {
        (
            compose_file_rank(p),
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        )
    });
    out
}

pub(super) fn detect_playwright_suggestions(root: &std::path::Path) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut pw_dirs: Vec<std::path::PathBuf> = Vec::new();
    for c in &[
        "playwright.config.ts",
        "playwright.config.js",
        "playwright.config.mjs",
    ] {
        if root.join(c).exists() {
            pw_dirs.push(root.to_path_buf());
            break;
        }
    }
    if pw_dirs.is_empty() {
        if let Ok(entries) = std::fs::read_dir(root) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    for c in &["playwright.config.ts", "playwright.config.js"] {
                        if p.join(c).exists() {
                            pw_dirs.push(p.clone());
                            break;
                        }
                    }
                }
            }
        }
    }
    for pw_dir in &pw_dirs {
        let is_root = pw_dir == root;
        let cwd_val: Value = if is_root {
            Value::Null
        } else {
            json!(pw_dir.to_string_lossy())
        };
        let pkg_manager =
            if pw_dir.join("pnpm-lock.yaml").exists() || root.join("pnpm-lock.yaml").exists() {
                "pnpm"
            } else if pw_dir.join("yarn.lock").exists() || root.join("yarn.lock").exists() {
                "yarn"
            } else {
                "npm"
            };
        let dir_label = if is_root {
            "root".to_string()
        } else {
            pw_dir
                .strip_prefix(root)
                .ok()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|| {
                    pw_dir
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string()
                })
        };
        let prefix = if is_root {
            String::new()
        } else {
            format!("[{}] ", dir_label)
        };
        let group = format!("playwright/{}", dir_label);
        push_sugg(
            &mut out,
            format!("{}playwright test", prefix),
            "playwright",
            pkg_manager,
            vec![json!("exec"), json!("playwright"), json!("test")],
            cwd_val.clone(),
            json!({}),
            "test",
            false,
            group.clone(),
        );
        push_sugg(
            &mut out,
            format!("{}playwright test --update-snapshots", prefix),
            "playwright",
            pkg_manager,
            vec![
                json!("exec"),
                json!("playwright"),
                json!("test"),
                json!("--update-snapshots"),
            ],
            cwd_val.clone(),
            json!({}),
            "test",
            false,
            group.clone(),
        );
        for sub in &["tests", "e2e", "test"] {
            let tests_root = pw_dir.join(sub);
            if !tests_root.exists() {
                continue;
            }
            for spec in walkdir_specs(&tests_root).iter().take(10) {
                let rel = spec
                    .strip_prefix(if is_root { root } else { pw_dir })
                    .unwrap_or(spec)
                    .to_string_lossy()
                    .replace('\\', "/");
                let name = spec
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .trim_end_matches(".spec")
                    .to_string();
                push_sugg(
                    &mut out,
                    format!("{}playwright · {}", prefix, name),
                    "playwright",
                    pkg_manager,
                    vec![
                        json!("exec"),
                        json!("playwright"),
                        json!("test"),
                        json!(rel),
                    ],
                    cwd_val.clone(),
                    json!({}),
                    "test",
                    false,
                    group.clone(),
                );
            }
        }
    }
    out
}

/// 0 = dev, 1 = local, 2 = base (nessun suffisso), 3 = prod/altro.
pub(super) fn compose_file_rank(p: &std::path::Path) -> u8 {
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let stem = name
        .trim_end_matches(".yml")
        .trim_end_matches(".yaml")
        .to_string();
    // stem tipo: docker-compose, docker-compose.dev, compose.prod, ecc.
    let suffix = stem.rsplit('.').next().unwrap_or("");
    match suffix {
        "dev" | "development" => 0,
        "local" | "override" => 1,
        "prod" | "production" | "staging" | "ci" => 3,
        "" => 2,
        _ => {
            // Se il suffisso è l'intero stem → è il file base (es. "compose", "docker-compose").
            if suffix == stem {
                2
            } else {
                3
            }
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
    out: &mut Vec<Value>,
    label: String,
    kind: &str,
    command: &str,
    args: Vec<Value>,
    cwd: Value,
    env: Value,
    role: &str,
    essential: bool,
    group: String,
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
        std::fs::read_dir(dir)
            .ok()
            .map(|d| {
                d.flatten()
                    .any(|e| e.path().extension().map(|x| x == "sln").unwrap_or(false))
            })
            .unwrap_or(false)
    }

    fn classify_csproj(path: &std::path::Path) -> Option<&'static str> {
        let content = std::fs::read_to_string(path).unwrap_or_default();
        let name_lc = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if name_lc.contains("test")
            || name_lc.contains("spec")
            || content.contains("xunit")
            || content.contains("nunit")
            || content.contains("MSTest")
        {
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
                let label = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
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
    let group_suffix = if containerized {
        " (host — richiede SDK locale)"
    } else {
        " (richiede .NET SDK)"
    };

    let mut out: Vec<Value> = Vec::new();
    for (sln_dir, dir_label) in &sln_dirs {
        let base_group = if dir_label.is_empty() {
            "dotnet".to_string()
        } else {
            dir_label.clone()
        };
        let group = format!("{}{}", base_group, group_suffix);
        let mut runnable: Vec<std::path::PathBuf> = Vec::new();
        let mut has_tests = false;

        if let Ok(entries) = std::fs::read_dir(sln_dir) {
            for e in entries.flatten() {
                let p = e.path();
                let search_dirs: Vec<std::path::PathBuf> =
                    if p.is_dir() { vec![p] } else { vec![] };
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
                let proj_dir = rel
                    .parent()
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
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
            let test_cmd = if dir_label.is_empty() {
                "dotnet test".to_string()
            } else {
                format!("dotnet test {}", dir_label)
            };
            let test_args: Vec<serde_json::Value> = if dir_label.is_empty() {
                vec![json!("test")]
            } else {
                vec![json!("test"), json!(dir_label.clone())]
            };
            out.push(
                json!({ "label": test_cmd, "kind": "shell", "command": "dotnet",
                "args": test_args, "cwd": null, "env": {},
                "role": "test", "essential": false, "group": group.clone() }),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ── A2: derivazione env frontend dai sibling (regola L, punto unico) ──

    #[test]
    fn derive_env_genera_vite_api_url_dal_backend_sibling() {
        // Caso incidente login Beauty-Book: un frontend Vite con un backend
        // sibling allocato deve ricevere VITE_API_URL sulla porta del backend
        // (prima Nexus non la generava -> proxy /api verso porta vuota -> 500).
        let mut env: HashMap<String, String> = HashMap::new();
        let siblings = vec![(21976, "backend".to_string())];
        derive_frontend_sibling_env(&mut env, &siblings, "pnpm run dev:frontend", "pnpm");
        assert_eq!(
            env.get("VITE_API_URL").map(String::as_str),
            Some("http://127.0.0.1:21976")
        );
        assert_eq!(
            env.get("BACKEND_API_URL").map(String::as_str),
            Some("http://127.0.0.1:21976")
        );
    }

    #[test]
    fn derive_env_label_api_prefix_riconosciuta() {
        // Il backend sibling puo' avere label "api-*" oltre a "backend-*".
        let mut env: HashMap<String, String> = HashMap::new();
        let siblings = vec![(30001, "frontend".to_string()), (30002, "api-main".to_string())];
        derive_frontend_sibling_env(&mut env, &siblings, "vite", "node");
        assert_eq!(
            env.get("VITE_API_URL").map(String::as_str),
            Some("http://127.0.0.1:30002")
        );
    }

    #[test]
    fn derive_env_non_sovrascrive_valore_esplicito() {
        // Guard !contains_key: un VITE_API_URL gia' impostato dall'utente resta.
        let mut env: HashMap<String, String> = HashMap::new();
        env.insert("VITE_API_URL".to_string(), "http://custom:9999".to_string());
        let siblings = vec![(21976, "backend".to_string())];
        derive_frontend_sibling_env(&mut env, &siblings, "vite", "pnpm");
        assert_eq!(
            env.get("VITE_API_URL").map(String::as_str),
            Some("http://custom:9999")
        );
    }

    #[test]
    fn derive_env_inerte_senza_backend_sibling() {
        // Nessun sibling backend -> niente VITE_API_URL/BACKEND_API_URL.
        let mut env: HashMap<String, String> = HashMap::new();
        let siblings = vec![(30001, "frontend".to_string())];
        derive_frontend_sibling_env(&mut env, &siblings, "vite", "pnpm");
        assert!(!env.contains_key("VITE_API_URL"));
        assert!(!env.contains_key("BACKEND_API_URL"));
    }

    #[test]
    fn derive_env_inerte_se_non_frontend() {
        // Un backend (kind node, exec senza marker frontend) non deve ricevere
        // VITE_API_URL anche se ha un sibling backend.
        let mut env: HashMap<String, String> = HashMap::new();
        let siblings = vec![(21976, "backend".to_string())];
        derive_frontend_sibling_env(&mut env, &siblings, "node src/server.js", "node");
        assert!(!env.contains_key("VITE_API_URL"));
        assert!(!env.contains_key("BACKEND_API_URL"));
    }

    #[test]
    fn derive_env_nextauth_per_next_dal_proprio_port() {
        // Next.js: NEXTAUTH_URL derivata dalla porta del frontend stesso (PORT).
        let mut env: HashMap<String, String> = HashMap::new();
        env.insert("PORT".to_string(), "21950".to_string());
        let siblings: Vec<(i32, String)> = vec![];
        derive_frontend_sibling_env(&mut env, &siblings, "next start", "npm");
        assert_eq!(
            env.get("NEXTAUTH_URL").map(String::as_str),
            Some("http://localhost:21950")
        );
    }

    #[test]
    fn inject_override_aggiunge_base_e_override_prima_di_up() {
        // Caso reale Beauty-Book: il suggerimento docker non ha -f esplicito.
        // Senza base, `docker compose -f override.yml up` perde il discovery del
        // file principale -> nessun servizio. Devono comparire ENTRAMBI.
        let r = inject_override_flag(
            "/usr/bin/docker compose up --build",
            "docker-compose.yml",
            "docker-compose.nexus.yml",
        );
        assert_eq!(
            r,
            "/usr/bin/docker compose -f docker-compose.yml -f docker-compose.nexus.yml up --build"
        );
    }

    #[test]
    fn inject_override_rispetta_il_compose_custom_dellutente() {
        // Se l'utente ha gia' indicato -f, non sovrascriviamo: aggiungiamo solo
        // l'override sopra la sua scelta.
        let r = inject_override_flag(
            "docker compose -f docker-compose.dev.yml up",
            "docker-compose.yml",
            "docker-compose.nexus.yml",
        );
        assert_eq!(
            r,
            "docker compose -f docker-compose.dev.yml -f docker-compose.nexus.yml up"
        );
    }

    #[test]
    fn inject_override_idempotente() {
        let already = "docker compose -f docker-compose.yml -f docker-compose.nexus.yml up";
        assert_eq!(
            inject_override_flag(already, "docker-compose.yml", "docker-compose.nexus.yml"),
            already
        );
    }

    #[test]
    fn inject_override_aggiunge_base_se_override_e_unico_f() {
        // Caso reale Beauty-Book: l'ExecStart conteneva SOLO l'override come `-f`
        // (senza base) -> mancavano image/build/volumes. Il base va anteposto.
        let r = inject_override_flag(
            "/usr/bin/docker compose -f docker-compose.nexus.yml up --build",
            "docker-compose.yml",
            "docker-compose.nexus.yml",
        );
        assert_eq!(
            r,
            "/usr/bin/docker compose -f docker-compose.yml -f docker-compose.nexus.yml up --build"
        );
    }

    #[test]
    fn inject_override_preserva_base_custom_con_override() {
        // base custom (dev) + override gia' presenti -> invariato (non forziamo
        // docker-compose.yml sopra la scelta dell'utente).
        let already = "docker compose -f docker-compose.dev.yml -f docker-compose.nexus.yml up";
        assert_eq!(
            inject_override_flag(already, "docker-compose.yml", "docker-compose.nexus.yml"),
            already
        );
    }

    #[test]
    fn rileva_bus_non_disponibile_connection_refused() {
        // Output tipico di WSL senza user manager.
        assert!(systemd_bus_unavailable(
            "Failed to connect to bus: Connection refused"
        ));
    }

    #[test]
    fn rileva_bus_non_disponibile_case_insensitive() {
        assert!(systemd_bus_unavailable("FAILED TO CONNECT TO BUS"));
        assert!(systemd_bus_unavailable("connection refused"));
    }

    #[test]
    fn rileva_bus_non_disponibile_no_such_file() {
        // XDG_RUNTIME_DIR/bus inesistente.
        assert!(systemd_bus_unavailable(
            "Failed to connect to bus: No such file or directory"
        ));
    }

    #[test]
    fn stato_running_non_e_errore_bus() {
        // Output normale di un manager attivo non deve essere classificato
        // come bus non disponibile.
        assert!(!systemd_bus_unavailable("running"));
        assert!(!systemd_bus_unavailable("degraded"));
        assert!(!systemd_bus_unavailable(""));
    }
}
