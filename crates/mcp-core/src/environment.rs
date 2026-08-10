use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

use crate::AppState;

type ApiError = (StatusCode, Json<Value>);
type ApiResult = Result<Json<Value>, ApiError>;

fn api_error(status: StatusCode, message: impl Into<String>) -> ApiError {
    (status, Json(json!({ "error": message.into() })))
}

#[derive(Debug, Serialize)]
pub struct EnvironmentCheck {
    pub id: String,
    pub label: String,
    pub status: String, // "ok" | "warn" | "error"
    pub detail: String,
}

impl EnvironmentCheck {
    fn ok(id: &str, label: &str, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            status: "ok".into(),
            detail: detail.into(),
        }
    }
    fn warn(id: &str, label: &str, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            status: "warn".into(),
            detail: detail.into(),
        }
    }
    fn error(id: &str, label: &str, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            status: "error".into(),
            detail: detail.into(),
        }
    }
}

async fn check_db(db: &sqlx::PgPool) -> EnvironmentCheck {
    match sqlx::query("SELECT 1").fetch_one(db).await {
        Ok(_) => EnvironmentCheck::ok("db", "PostgreSQL", "Connected"),
        Err(e) => EnvironmentCheck::error("db", "PostgreSQL", format!("{e}")),
    }
}

#[cfg(unix)]
async fn check_playwright_libs() -> EnvironmentCheck {
    // Su Linux Chromium (Playwright) dipende da librerie .so di sistema (libatk,
    // libnss, ecc.). Prova ldconfig -p; in fallback cerca in /usr/lib.
    let found = if let Ok(out) = Command::new("ldconfig").args(["-p"]).output().await {
        let stdout = String::from_utf8_lossy(&out.stdout);
        stdout.contains("libatk")
    } else {
        // Fallback: find /usr/lib -name "libatk*"
        Command::new("find")
            .args(["/usr/lib", "-name", "libatk*", "-maxdepth", "3"])
            .output()
            .await
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false)
    };

    if found {
        EnvironmentCheck::ok(
            "playwright_libs",
            "Playwright system libs",
            "libatk-1.0.so.0 found",
        )
    } else {
        EnvironmentCheck::error(
            "playwright_libs",
            "Playwright system libs",
            "libatk-1.0.so.0 missing",
        )
    }
}

#[cfg(windows)]
async fn check_playwright_libs() -> EnvironmentCheck {
    // Su Windows Chromium (Playwright) non richiede librerie .so di sistema:
    // le dipendenze native sono incluse nel bundle del browser. Nessun check
    // ldconfig/find applicabile: lo stato e' sempre OK per non generare falsi
    // allarmi "libreria mancante".
    EnvironmentCheck::ok(
        "playwright_libs",
        "Playwright system libs",
        "nessuna libreria di sistema richiesta su Windows",
    )
}

async fn check_playwright_browser() -> EnvironmentCheck {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let cache_dir = format!("{}/.cache/ms-playwright", home);

    let found = if let Ok(mut rd) = tokio::fs::read_dir(&cache_dir).await {
        let mut has_entry = false;
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name();
            let n = name.to_string_lossy();
            if n.starts_with("chromium") {
                has_entry = true;
                break;
            }
        }
        has_entry
    } else {
        false
    };

    if found {
        EnvironmentCheck::ok("playwright_browser", "Chromium browser", "Installed")
    } else {
        EnvironmentCheck::error("playwright_browser", "Chromium browser", "not installed")
    }
}

/// Verifica che il gRPC ToolRunner sia in ascolto sulla porta configurata.
/// Indirizzo letto da: env var TOOL_RUNNER_ADDR (override) > DB settings
/// (canonico) > hardcoded 127.0.0.1:50071.
/// Senza questo gRPC il brain Python non puo' invocare i tool MCP (read_file,
/// str_replace, ecc.) e l'AI fallisce con "0 step" o "tool gRPC unreachable".
async fn check_tool_runner(db: &sqlx::PgPool) -> EnvironmentCheck {
    let db_addr = crate::settings::get_setting(db, "tool_runner_addr")
        .await
        .ok()
        .flatten()
        .map(|v| v.trim().to_string());
    let addr = std::env::var("TOOL_RUNNER_ADDR")
        .ok()
        .or(db_addr)
        .unwrap_or_else(|| "127.0.0.1:50071".into());
    let host_port: Vec<&str> = addr.split(':').collect();
    let port = host_port
        .get(1)
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(50071);
    // Tentativo di TCP connect non bloccante (timeout 1s)
    let connect = tokio::time::timeout(
        Duration::from_millis(1000),
        tokio::net::TcpStream::connect(format!("127.0.0.1:{}", port)),
    )
    .await;
    match connect {
        Ok(Ok(_)) => EnvironmentCheck::ok(
            "tool_runner",
            "MCP Tools (gRPC)",
            format!("listening on :{}", port),
        ),
        Ok(Err(_)) | Err(_) => EnvironmentCheck::error(
            "tool_runner",
            "MCP Tools (gRPC)",
            format!("port :{} not reachable — l'AI non potrà usare i tool", port),
        ),
    }
}

/// Probe TCP portabile: `true` se `127.0.0.1:{port}` accetta una connessione
/// entro 1s. Un connect riuscito implica porta in ascolto, indipendentemente
/// dall'OS (regola H: niente `ss`/`netstat` POSIX-only). Punto unico riusato da
/// tutti i check di porta di questo modulo (regola L).
async fn tcp_port_open(port: u16) -> bool {
    timeout(
        Duration::from_secs(1),
        tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

/// Sonda in parallelo i microservizi e restituisce `(nome, in_ascolto)` nello
/// stesso ordine della lista in ingresso.
async fn probe_microservices(services: &[(&'static str, u16)]) -> Vec<(&'static str, bool)> {
    let handles: Vec<_> = services
        .iter()
        .map(|(name, port)| {
            let p = *port;
            (*name, tokio::spawn(async move { tcp_port_open(p).await }))
        })
        .collect();

    let mut results: Vec<(&str, bool)> = Vec::with_capacity(handles.len());
    for (name, handle) in handles {
        results.push((name, handle.await.unwrap_or(false)));
    }
    results
}

/// Controlla i microservizi Rust ausiliari (admin, doc, plugin).
/// Li verifica in parallelo con TCP connect (1s timeout); restituisce un check
/// aggregato con il dettaglio per ciascun servizio.
async fn check_microservices() -> EnvironmentCheck {
    const LABEL: &str = "Microservizi (admin/doc/plugin)";
    let services = [
        ("admin-service", 4010u16),
        ("doc-service", 4030),
        ("plugin-service", 4050),
    ];

    let results = probe_microservices(&services).await;
    let ok_count = results.iter().filter(|(_, ok)| *ok).count();
    let total = results.len();
    let detail = results
        .iter()
        .map(|(name, ok)| format!("{}: {}", name, if *ok { "ok" } else { "down" }))
        .collect::<Vec<_>>()
        .join(", ");

    if ok_count == total {
        EnvironmentCheck::ok(
            "microservices",
            LABEL,
            format!("{ok_count}/{total} operativi"),
        )
    } else if ok_count > 0 {
        EnvironmentCheck::warn(
            "microservices",
            LABEL,
            format!("{ok_count}/{total} operativi — {detail}"),
        )
    } else {
        EnvironmentCheck::error(
            "microservices",
            LABEL,
            format!("0/{total} operativi — {detail}"),
        )
    }
}

fn check_backend_process() -> EnvironmentCheck {
    let pid = std::process::id();
    EnvironmentCheck::ok("backend_process", "Backend mcp-core", format!("pid {pid}"))
}

async fn check_frontend_process() -> EnvironmentCheck {
    // Probe TCP portabile (regola H+L): riusa `tcp_port_open`. Un connect
    // riuscito su 127.0.0.1:3000 implica porta in ascolto, indipendentemente
    // dall'OS. Elimina la dipendenza da `ss` (POSIX-only, falso allarme "down"
    // su Windows). Non piu' disponibili pid/program: solo lo stato in-ascolto.
    const FRONTEND_PORT: u16 = 3000;
    if tcp_port_open(FRONTEND_PORT).await {
        EnvironmentCheck::ok(
            "frontend_process",
            "Frontend web-ide",
            format!("Port {FRONTEND_PORT} listening"),
        )
    } else {
        EnvironmentCheck::error(
            "frontend_process",
            "Frontend web-ide",
            format!("Port {FRONTEND_PORT} not listening"),
        )
    }
}

/// Check `warn` standard quando sqlx-cli non e' installato/trovato.
fn migrations_sqlx_missing() -> EnvironmentCheck {
    EnvironmentCheck::warn(
        "migrations_sqlx_missing",
        "DB Migrations",
        "sqlx-cli non installato",
    )
}

/// Risolve il path di sqlx-cli in modo cross-platform.
/// - Unix: path esplicito noto, poi `which` sul PATH.
/// - Windows: `where` (where.exe) sul PATH; niente path esplicito
///   (l'installazione cargo mette sqlx.exe in %USERPROFILE%\.cargo\bin, gia'
///   nel PATH). `which` non esiste su Windows: usarlo darebbe un falso
///   "sqlx-cli non installato".
/// Ritorna `Err(check)` con il warn gia' pronto se non risolvibile.
#[cfg(unix)]
async fn resolve_sqlx_path() -> Result<String, EnvironmentCheck> {
    if std::path::Path::new("/home/administrator/.cargo/bin/sqlx").exists() {
        return Ok("/home/administrator/.cargo/bin/sqlx".to_string());
    }
    let which_out = Command::new("which").arg("sqlx").output().await;
    match which_out {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        _ => Err(migrations_sqlx_missing()),
    }
}

/// Vedi doc di `resolve_sqlx_path` (variante Windows).
#[cfg(windows)]
async fn resolve_sqlx_path() -> Result<String, EnvironmentCheck> {
    let where_out = Command::new("where").arg("sqlx").output().await;
    match where_out {
        // `where` puo' stampare piu' righe (path multipli): prende la prima.
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            match stdout.lines().next().map(|l| l.trim().to_string()) {
                Some(p) if !p.is_empty() => Ok(p),
                _ => Err(migrations_sqlx_missing()),
            }
        }
        _ => Err(migrations_sqlx_missing()),
    }
}

async fn check_migrations(db_url: &str) -> EnvironmentCheck {
    let sqlx_path = match resolve_sqlx_path().await {
        Ok(p) => p,
        Err(check) => return check,
    };

    let result = timeout(
        Duration::from_secs(10),
        Command::new(&sqlx_path)
            .args(["migrate", "info", "--database-url", db_url])
            .output(),
    )
    .await;

    match result {
        Ok(Ok(out)) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let pending: usize = stdout
                .lines()
                .filter(|l| l.to_lowercase().contains("pending"))
                .count();
            if pending == 0 {
                EnvironmentCheck::ok("migrations", "DB Migrations", "Up to date")
            } else {
                EnvironmentCheck::warn("migrations", "DB Migrations", format!("{pending} pending"))
            }
        }
        Ok(Err(_)) => EnvironmentCheck::warn(
            "migrations_sqlx_missing",
            "DB Migrations",
            "sqlx-cli non trovato",
        ),
        Err(_) => EnvironmentCheck::warn("migrations", "DB Migrations", "sqlx timeout"),
    }
}

async fn check_ai_providers(db: &sqlx::PgPool) -> EnvironmentCheck {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM settings WHERE key LIKE '%_api_key' AND value != '' AND value IS NOT NULL"
    )
    .fetch_one(db)
    .await
    .unwrap_or(0);

    if count > 0 {
        EnvironmentCheck::ok(
            "ai_providers",
            "AI Providers",
            format!("{count} providers configured"),
        )
    } else {
        EnvironmentCheck::warn("ai_providers", "AI Providers", "0 providers configured")
    }
}

#[cfg(unix)]
async fn check_disk_space() -> EnvironmentCheck {
    let result = Command::new("df").args(["-h", "/"]).output().await;
    match result {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            // Parse the second line of df output
            // Filesystem      Size  Used Avail Use% Mounted on
            // /dev/sda1        50G   23G   27G  46% /
            let line = stdout.lines().nth(1).unwrap_or("");
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 5 {
                let avail = parts[3];
                let use_pct_str = parts[4].trim_end_matches('%');
                let use_pct: u32 = use_pct_str.parse().unwrap_or(0);
                let detail = format!("{avail} free ({use_pct}% used)");
                if use_pct >= 95 {
                    EnvironmentCheck::error("disk_space", "Disk space", detail)
                } else if use_pct >= 85 {
                    EnvironmentCheck::warn("disk_space", "Disk space", detail)
                } else {
                    EnvironmentCheck::ok("disk_space", "Disk space", detail)
                }
            } else {
                EnvironmentCheck::warn("disk_space", "Disk space", "could not parse df output")
            }
        }
        Err(e) => EnvironmentCheck::warn("disk_space", "Disk space", format!("df failed: {e}")),
    }
}

#[cfg(windows)]
async fn check_disk_space() -> EnvironmentCheck {
    // Degrado pulito su Windows: `df` non esiste. L'API nativa
    // GetDiskFreeSpaceExW richiederebbe la feature `Win32_Storage_FileSystem` di
    // windows-sys (non abilitata nel Cargo.toml, che espone solo Foundation +
    // System::Threading). Per non introdurre nuove dipendenze/feature e per non
    // generare un finto errore "df failed", si riporta uno stato esplicito di
    // metrica non disponibile (status ok: non e' un guasto, solo non misurabile).
    EnvironmentCheck::ok(
        "disk_space",
        "Disk space",
        "metrica non disponibile su Windows",
    )
}

pub async fn get_environment_status(State(state): State<AppState>) -> ApiResult {
    let db_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://nexus:nexus@localhost:5433/nexus".to_string());

    let (
        db_check,
        playwright_libs_check,
        playwright_browser_check,
        tool_runner_check,
        frontend_check,
        migrations_check,
        providers_check,
        disk_check,
        microservices_check,
    ) = tokio::join!(
        check_db(&state.db),
        check_playwright_libs(),
        check_playwright_browser(),
        check_tool_runner(&state.db),
        check_frontend_process(),
        check_migrations(&db_url),
        check_ai_providers(&state.db),
        check_disk_space(),
        check_microservices(),
    );

    let backend_check = check_backend_process();

    let checks = vec![
        db_check,
        playwright_libs_check,
        playwright_browser_check,
        tool_runner_check,
        backend_check,
        microservices_check,
        frontend_check,
        migrations_check,
        providers_check,
        disk_check,
    ];

    Ok(Json(json!({ "checks": checks })))
}

#[derive(Debug, Deserialize)]
pub struct FixRequest {
    pub action: String,
    pub sudo_password: Option<String>,
}

/// Serializza in JSON l'esito di un `Command`: `stdout`+`stderr` concatenati e
/// `ok` dallo status. `timeout_msg`/`spawn_prefix` personalizzano i messaggi di
/// errore per riprodurre esattamente le stringhe originali di ogni azione.
fn command_result_json(
    result: Result<std::io::Result<std::process::Output>, tokio::time::error::Elapsed>,
    spawn_prefix: &str,
    timeout_msg: &str,
) -> ApiResult {
    match result {
        Ok(Ok(out)) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let output = format!("{stdout}{stderr}");
            Ok(Json(
                json!({ "ok": out.status.success(), "output": output }),
            ))
        }
        Ok(Err(e)) => Ok(Json(
            json!({ "ok": false, "output": format!("{spawn_prefix}{e}") }),
        )),
        Err(_) => Ok(Json(json!({ "ok": false, "output": timeout_msg }))),
    }
}

async fn action_install_playwright_browsers() -> ApiResult {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let result = timeout(
        Duration::from_secs(120),
        Command::new("npx")
            .args(["playwright", "install", "chromium"])
            .current_dir(&home)
            .output(),
    )
    .await;
    command_result_json(result, "Error: ", "Timeout after 120s")
}

async fn action_run_migrations() -> ApiResult {
    let db_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://nexus:nexus@localhost:5433/nexus".to_string());
    let result = timeout(
        Duration::from_secs(60),
        Command::new("/home/administrator/.cargo/bin/sqlx")
            .args(["migrate", "run", "--database-url", &db_url])
            .output(),
    )
    .await;
    command_result_json(result, "Error: ", "Timeout after 60s")
}

async fn action_restart_frontend() -> ApiResult {
    #[cfg(unix)]
    {
        // Kill processo sulla porta 3000
        let _ = Command::new("sh")
            .args(["-c", "kill $(ss -tlnp | grep ':3000' | grep -oP 'pid=\\K[0-9]+' | head -1) 2>/dev/null || true"])
            .output()
            .await;

        tokio::time::sleep(Duration::from_secs(1)).await;

        // Cerca la directory del frontend
        let nexus_root = std::env::var("NEXUS_ROOT")
            .unwrap_or_else(|_| "/var/lib/postgresql/wal/nexus".to_string());
        let frontend_dir = format!("{nexus_root}/apps/web-ide");

        let result = Command::new("sh")
            .args([
                "-c",
                &format!("cd {frontend_dir} && nohup pnpm start > /tmp/web-ide.log 2>&1 &"),
            ])
            .output()
            .await;

        match result {
            Ok(_) => Ok(Json(
                json!({ "ok": true, "output": "Frontend restart initiated. Check port 3000 in a few seconds." }),
            )),
            Err(e) => Ok(Json(
                json!({ "ok": false, "output": format!("Error: {e}") }),
            )),
        }
    }
    #[cfg(windows)]
    {
        // Su Windows il web-ide gira come servizio WinSW (nexus-web-ide):
        // il restart e' gestito dal service manager, non da qui. No-op.
        tracing::warn!(
            "restart frontend: su Windows il web-ide e' gestito da WinSW (nexus-web-ide), no-op"
        );
        Ok(Json(json!({
            "ok": true,
            "output": "Su Windows il web-ide e' gestito da WinSW (nexus-web-ide): usa il service manager. No-op."
        })))
    }
}

#[cfg(unix)]
async fn install_system_deps_unix(sudo_password: &str) -> ApiResult {
    if sudo_password.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "sudo_password required"));
    }

    let packages = "libatk1.0-0 libatk-bridge2.0-0 libcups2 libxcomposite1 libxdamage1 libxfixes3 libxrandr2 libgbm1 libpango-1.0-0 libcairo2 libasound2t64 libnspr4 libnss3 libx11-xcb1 libxcb-dri3-0 libdrm2 libglib2.0-0 libdbus-1-3 libxshmfence1 libxext6";

    let cmd = format!(
        "echo '{}' | sudo -S apt-get install -y {} 2>&1",
        sudo_password, packages
    );

    let result = tokio::time::timeout(
        Duration::from_secs(120),
        tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let combined = format!("{}\n{}", stdout, stderr)
                .lines()
                .filter(|l| !l.contains("[sudo] password") && !l.contains("password for"))
                .collect::<Vec<_>>()
                .join("\n");
            let ok = output.status.success();
            Ok(Json(json!({ "ok": ok, "output": combined })))
        }
        Ok(Err(e)) => Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
        Err(_) => Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, "Timeout")),
    }
}

async fn action_install_system_deps(_sudo_password: Option<&str>) -> ApiResult {
    #[cfg(unix)]
    {
        install_system_deps_unix(_sudo_password.unwrap_or("")).await
    }
    #[cfg(windows)]
    {
        // Niente apt-get/sudo su Windows: l'installazione delle dipendenze
        // di sistema (librerie native per Playwright/Chromium ecc.) non e'
        // automatizzabile qui. Segnala chiaramente all'utente.
        tracing::warn!(
            "install_system_deps: non supportato su Windows, installazione manuale richiesta"
        );
        Ok(Json(json!({
            "ok": false,
            "output": "installazione dipendenze di sistema non supportata su Windows: installale manualmente"
        })))
    }
}

async fn action_install_sqlx_cli() -> ApiResult {
    // Installa sqlx-cli con supporto solo postgres (più veloce, ~2-3 min).
    // Usa il cargo del sistema (PATH ereditato da mcp-core) oppure il path
    // esplicito ~/.cargo/bin/cargo come fallback.
    let cargo_bin = if std::path::Path::new("/home/administrator/.cargo/bin/cargo").exists() {
        "/home/administrator/.cargo/bin/cargo".to_string()
    } else {
        "cargo".to_string()
    };

    let result = timeout(
        Duration::from_secs(300), // 5 minuti: compilazione da sorgente
        Command::new(&cargo_bin)
            .args([
                "install",
                "sqlx-cli",
                "--no-default-features",
                "--features",
                "native-tls,postgres",
                "--locked",
            ])
            .envs(std::env::vars()) // propaga PATH, CARGO_HOME, ecc.
            .output(),
    )
    .await;
    command_result_json(
        result,
        "Errore avvio cargo: ",
        "Timeout dopo 300s. Riprova.",
    )
}

pub async fn fix_environment(
    State(_state): State<AppState>,
    Json(body): Json<FixRequest>,
) -> ApiResult {
    match body.action.as_str() {
        "install_playwright_browsers" => action_install_playwright_browsers().await,
        "run_migrations" => action_run_migrations().await,
        "get_system_deps_command" => Ok(Json(json!({
            "ok": true,
            "output": "sudo apt-get install -y libatk1.0-0 libatk-bridge2.0-0 libcups2 libxcomposite1 libxdamage1 libxfixes3 libxrandr2 libgbm1 libpango-1.0-0 libcairo2 libasound2t64 libnspr4 libnss3 libx11-xcb1 libxcb-dri3-0 libdrm2 libglib2.0-0"
        }))),
        "restart_frontend" => action_restart_frontend().await,
        "install_system_deps" => action_install_system_deps(body.sudo_password.as_deref()).await,
        "install_sqlx_cli" => action_install_sqlx_cli().await,
        _ => Err(api_error(
            StatusCode::BAD_REQUEST,
            format!("Unknown action: {}", body.action),
        )),
    }
}

pub async fn qdrant_health_handler(
    axum::extract::State(_state): axum::extract::State<crate::AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let qdrant_url =
        crate::settings::disambigua_loopback(
            &std::env::var("QDRANT_URL").unwrap_or_else(|_| "http://localhost:6333".to_string()),
        );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap_or_default();

    let health_url = format!("{}/healthz", qdrant_url.trim_end_matches('/'));
    let collections_url = format!("{}/collections", qdrant_url.trim_end_matches('/'));

    let (healthy, error) = match client.get(&health_url).send().await {
        Ok(r) if r.status().is_success() => (true, None::<String>),
        Ok(r) => (false, Some(format!("HTTP {}", r.status()))),
        Err(e) => (false, Some(e.to_string())),
    };

    let collections: usize = if healthy {
        match client.get(&collections_url).send().await {
            Ok(r) => match r.json::<serde_json::Value>().await {
                Ok(v) => v["result"]["collections"]
                    .as_array()
                    .map(|a| a.len())
                    .unwrap_or(0),
                Err(_) => 0,
            },
            Err(_) => 0,
        }
    } else {
        0
    };

    let mut result = json!({ "healthy": healthy, "url": qdrant_url, "collections": collections });
    if let Some(err) = error {
        result["error"] = serde_json::Value::String(err);
    }
    Ok(Json(result))
}

/// URL del gateway risolto dalla porta nel DB (regola G: niente env/hardcoded).
/// A runtime non si panica: se il DB e' down o la chiave manca si ritorna 503.
async fn resolve_gateway_url(
    db: &sqlx::PgPool,
) -> Result<String, (StatusCode, Json<serde_json::Value>)> {
    match crate::settings::get_setting(db, "nexus_gateway_port").await {
        Ok(Some(v)) => match v.trim().parse::<u16>() {
            Ok(p) if p > 0 => Ok(format!("http://127.0.0.1:{p}")),
            _ => Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": format!("settings.nexus_gateway_port = {v:?} non e' una porta valida")
                })),
            )),
        },
        Ok(None) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "settings.nexus_gateway_port assente nel DB. Verifica la migrazione 0239."
            })),
        )),
        Err(e) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({ "error": format!("lettura nexus_gateway_port fallita: {e}") }),
            ),
        )),
    }
}

/// Lista canonica dei provider noti (regola L: unica definizione riusata dagli
/// handler di stato provider di questo modulo).
const KNOWN_PROVIDERS: [&str; 5] = ["anthropic", "openai", "google", "deepseek", "mistral"];

/// Ultimo health check per provider (popolato dal worker `provider_health_probe`).
/// Punto unico riusato da `gateway_providers_handler` e `providers_status_internal`.
#[derive(sqlx::FromRow)]
struct ProviderHealthRow {
    provider: String,
    healthy: bool,
    latency_ms: Option<i32>,
    error_kind: Option<String>,
    // Messaggio diagnostico completo (regola M: la CAUSA, non solo la
    // categoria). Colonna presente in `nexus_provider_health_history` fin
    // dalla mig 0097 e sempre scritta dai due writer (probe periodico
    // mcp-core, errori su richiesta reale nexus-gateway), ma prima di
    // questo fix nessuna query la leggeva: un `healthy=false` restava senza
    // causa diagnosticabile appena il provider usciva dalla cooldown_map
    // in-process (vedi `providers_status_internal`, che copriva l'assenza
    // solo mentre il cooldown mcp-core era attivo).
    error_message: Option<String>,
    checked_at: chrono::DateTime<chrono::Utc>,
}

/// Carica l'ultimo health check per provider come mappa `provider -> riga`.
/// DISTINCT ON e' un'estensione PostgreSQL: prende per ogni provider la riga
/// piu' recente in O(N log N) sull'indice (provider, checked_at DESC).
async fn fetch_provider_health_map(
    db: &sqlx::PgPool,
) -> std::collections::HashMap<String, ProviderHealthRow> {
    let rows: Vec<ProviderHealthRow> = sqlx::query_as::<_, ProviderHealthRow>(
        r#"SELECT DISTINCT ON (provider)
                  provider, healthy, latency_ms, error_kind, error_message, checked_at
           FROM nexus_provider_health_history
           ORDER BY provider, checked_at DESC"#,
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();
    rows.into_iter().map(|r| (r.provider.clone(), r)).collect()
}

/// Mappa `provider -> API key configurata` (chiave `*_api_key` non vuota in
/// `settings`, categoria providers). `pub(crate)`: riusata dall'orchestrator per
/// derivare l'elenco provider dell'alert cooldown (punto unico, regola L).
pub(crate) async fn fetch_api_key_configured(
    db: &sqlx::PgPool,
) -> std::collections::HashMap<String, bool> {
    #[derive(sqlx::FromRow)]
    struct SettingsRow {
        key: String,
        value: String,
    }
    let rows: Vec<SettingsRow> = sqlx::query_as::<_, SettingsRow>(
        "SELECT key, value FROM settings WHERE category = 'providers' AND key LIKE '%_api_key'",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .map(|r| {
            (
                r.key.trim_end_matches("_api_key").to_string(),
                !r.value.trim().is_empty(),
            )
        })
        .collect()
}

/// Nomi dei provider da mostrare nello status/dashboard (regola G/L): unione dei
/// provider con almeno un modello abilitato nel catalog e di quelli con `*_api_key`
/// configurata in `settings`. Un provider onboardato (catalog o chiave) compare
/// senza toccare il codice (chiude T4). Fallback ai noti (`KNOWN_PROVIDERS`) se
/// entrambe le fonti sono vuote (DB down / bootstrap incompleto).
pub(crate) async fn provider_names_for_status(
    db: &sqlx::PgPool,
    api_key_configured: &std::collections::HashMap<String, bool>,
) -> Vec<String> {
    let from_catalog: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT provider FROM ai_price_catalog WHERE is_enabled = true",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();
    // Provider del registry (nexus_provider_registry, mig 0565): sempre visibili
    // se attivi, anche senza key ne' modelli abilitati, cosi' la dashboard admin
    // li mostra (LED "mai misurato") e permette di configurarli. Chiude T4.
    let from_registry: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM nexus_provider_registry WHERE is_active = true",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();
    merge_provider_names(from_catalog, from_registry, api_key_configured)
}

/// Unione ordinata + deduplicata di provider da catalog, registry e api_key
/// configurata; fallback ai noti se tutte vuote. Puro: testabile senza DB.
fn merge_provider_names(
    from_catalog: Vec<String>,
    from_registry: Vec<String>,
    api_key_configured: &std::collections::HashMap<String, bool>,
) -> Vec<String> {
    let mut set: std::collections::BTreeSet<String> = from_catalog.into_iter().collect();
    set.extend(from_registry);
    for (name, configured) in api_key_configured {
        if *configured {
            set.insert(name.clone());
        }
    }
    if set.is_empty() {
        return KNOWN_PROVIDERS.iter().map(|s| s.to_string()).collect();
    }
    set.into_iter().collect()
}

#[cfg(test)]
mod provider_names_tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn merge_unisce_catalog_e_chiavi_e_deduplica() {
        let mut keys = HashMap::new();
        keys.insert("openai".to_string(), true);
        keys.insert("perplexity".to_string(), true); // provider nuovo, solo chiave
        keys.insert("disattivato".to_string(), false); // non configurato -> escluso
        let out = merge_provider_names(vec!["openai".into(), "mistral".into()], vec![], &keys);
        // Ordinato, dedup (openai una volta), perplexity incluso, disattivato no.
        assert_eq!(out, vec!["mistral", "openai", "perplexity"]);
    }

    #[test]
    fn merge_include_registry_senza_chiave_ne_catalog() {
        let empty: HashMap<String, bool> = HashMap::new();
        // Un provider del registry attivo (groq) compare anche senza catalog
        // abilitato ne' api_key configurata: la dashboard deve poterlo mostrare.
        let out = merge_provider_names(vec!["openai".into()], vec!["groq".into()], &empty);
        assert_eq!(out, vec!["groq", "openai"]);
    }

    #[test]
    fn merge_fallback_ai_noti_se_vuoto() {
        let empty: HashMap<String, bool> = HashMap::new();
        let out = merge_provider_names(vec![], vec![], &empty);
        let expected: Vec<String> = KNOWN_PROVIDERS.iter().map(|s| s.to_string()).collect();
        assert_eq!(out, expected);
    }

    fn budget_row(provider: &str, budget: &str) -> BudgetRow {
        BudgetRow {
            provider: provider.to_string(),
            monthly_budget_usd: budget.to_string(),
            spent_current_period_usd: "3.5".to_string(),
            remaining_usd: "16.5".to_string(),
            min_threshold_usd: "1.0".to_string(),
            is_exhausted: false,
            period_start: chrono::Utc::now(),
        }
    }

    #[test]
    fn budget_provider_nuovo_senza_riga_appare_non_configurato() {
        let names = vec!["anthropic".to_string(), "groq".to_string()];
        let rows = vec![budget_row("anthropic", "20.0")];
        let out = merge_budget_entries(&names, rows);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["provider"], "anthropic");
        assert_eq!(out[0]["configured"], true);
        assert_eq!(out[0]["monthly_budget_usd"], "20.0");
        // groq: entry sintetica a budget 0, non configurata, non esausta.
        assert_eq!(out[1]["provider"], "groq");
        assert_eq!(out[1]["configured"], false);
        assert_eq!(out[1]["monthly_budget_usd"], "0");
        assert_eq!(out[1]["is_exhausted"], false);
    }

    #[test]
    fn budget_riga_orfana_preservata_in_coda() {
        // Provider con budget ma rimosso dalle fonti (nessun nome corrispondente):
        // resta visibile per non nascondere spesa gia' tracciata.
        let names = vec!["openai".to_string()];
        let rows = vec![budget_row("legacy-provider", "10.0")];
        let out = merge_budget_entries(&names, rows);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["provider"], "openai");
        assert_eq!(out[0]["configured"], false);
        assert_eq!(out[1]["provider"], "legacy-provider");
        assert_eq!(out[1]["configured"], true);
    }

    #[test]
    fn budget_ordinamento_deterministico_segue_names() {
        let names = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let out = merge_budget_entries(&names, vec![budget_row("b", "5.0")]);
        let ordine: Vec<&str> = out.iter().map(|v| v["provider"].as_str().unwrap()).collect();
        assert_eq!(ordine, vec!["a", "b", "c"]);
    }

    /// Catalogo vuoto: questi test misurano la resa dell'OSSERVAZIONE, che ha
    /// la precedenza sulla prontezza — il classificatore non li tocca.
    fn catalog_facts(
    ) -> std::collections::HashMap<String, Vec<crate::provider_readiness::ModelFact>> {
        std::collections::HashMap::new()
    }

    fn health_row(healthy: bool, error_kind: Option<&str>, error_message: Option<&str>) -> ProviderHealthRow {
        ProviderHealthRow {
            provider: "deepseek".to_string(),
            healthy,
            latency_ms: None,
            error_kind: error_kind.map(str::to_string),
            error_message: error_message.map(str::to_string),
            checked_at: chrono::Utc::now(),
        }
    }

    /// Il difetto misurato il 31/07/2026: un `healthy=false` senza
    /// cooldown mcp-core attivo (cooldown_map vuota — es. scaduto, o
    /// l'errore osservato solo dal gateway) arrivava a `/health` senza
    /// alcuna causa. Rompendo il fix (rimuovendo la copia di
    /// `error_message` in `build_provider_status_entry`), questo test
    /// rosseggia.
    #[test]
    fn unhealthy_senza_cooldown_espone_comunque_la_causa() {
        let mut health_map = std::collections::HashMap::new();
        health_map.insert(
            "deepseek".to_string(),
            health_row(false, Some("timeout"), Some("nessuna risposta in 30s")),
        );
        let cooldown_map = std::collections::HashMap::new(); // cooldown scaduto/mai applicato
        let api_keys = std::collections::HashMap::new();

        let entry = build_provider_status_entry("deepseek", &health_map, &cooldown_map, &api_keys, &catalog_facts());

        assert_eq!(entry["healthy"], json!(false));
        assert_eq!(entry["error_kind"], json!("timeout"));
        assert_eq!(entry["error"], json!("nessuna risposta in 30s"));
    }

    /// Se il cooldown mcp-core E' attivo, la sua reason resta prioritaria
    /// (piu' recente/specifica del messaggio del probe) — comportamento
    /// preesistente, non deve regredire con l'aggiunta del fallback.
    #[test]
    fn unhealthy_con_cooldown_attivo_usa_la_reason_del_cooldown() {
        let mut health_map = std::collections::HashMap::new();
        health_map.insert(
            "deepseek".to_string(),
            health_row(false, Some("timeout"), Some("messaggio del probe, piu' vecchio")),
        );
        let mut cooldown_map = std::collections::HashMap::new();
        cooldown_map.insert(
            "deepseek".to_string(),
            (42u64, Some("rate limit raggiunto ora".to_string())),
        );
        let api_keys = std::collections::HashMap::new();

        let entry = build_provider_status_entry("deepseek", &health_map, &cooldown_map, &api_keys, &catalog_facts());

        assert_eq!(entry["error"], json!("rate limit raggiunto ora"));
        assert_eq!(entry["cooldown_seconds_remaining"], json!(42));
    }

    /// healthy=true non deve mai portare un campo "error" fantasma.
    #[test]
    fn healthy_non_espone_errore() {
        let mut health_map = std::collections::HashMap::new();
        health_map.insert("deepseek".to_string(), health_row(true, None, None));
        let cooldown_map = std::collections::HashMap::new();
        let api_keys = std::collections::HashMap::new();

        let entry = build_provider_status_entry("deepseek", &health_map, &cooldown_map, &api_keys, &catalog_facts());

        assert_eq!(entry["healthy"], json!(true));
        assert!(entry.get("error").is_none());
    }
}

/// Applica a `p["error"]` la causa diagnosticabile di un provider unhealthy
/// (regola M: il messaggio pieno persistito con l'ultimo probe, non solo la
/// categoria). No-op se il provider e' sano o non c'e' un messaggio. Punto
/// unico (regola L) riusato dai tre costruttori di payload provider
/// (`build_provider_status_entry`, `build_providers_fallback`,
/// `apply_health_probe`): prima duplicato identico in ciascuno.
fn apply_health_error(p: &mut Value, h: &ProviderHealthRow) {
    if !h.healthy {
        if let Some(msg) = &h.error_message {
            p["error"] = json!(msg);
        }
    }
}

/// Snapshot dei provider in cooldown come mappa `provider -> (secondi, motivo)`.
fn fetch_cooldown_map() -> std::collections::HashMap<String, (u64, Option<String>)> {
    crate::provider_cooldown::cooldown_snapshot()
        .into_iter()
        .map(|(name, secs, reason)| (name, (secs, reason)))
        .collect()
}

/// Costruisce la lista fallback dei provider da health/api-key/cooldown map.
/// Usata quando il gateway TypeScript (4060) non e' raggiungibile, cosi' i LED
/// mostrano l'ultimo stato noto invece di essere tutti grigi.
fn build_providers_fallback(
    names: &[String],
    health_map: &std::collections::HashMap<String, ProviderHealthRow>,
    api_key_configured: &std::collections::HashMap<String, bool>,
    cooldown_map: &std::collections::HashMap<String, (u64, Option<String>)>,
    catalog_facts: &std::collections::HashMap<String, Vec<crate::provider_readiness::ModelFact>>,
) -> Vec<serde_json::Value> {
    names
        .iter()
        .map(|name| {
            let name = name.as_str();
            // Stessa prontezza dell'altro ramo, dallo stesso punto: il gateway
            // spento non cambia cio' che sappiamo della salute di un fornitore.
            let mut p = entry_con_prontezza(name, health_map, api_key_configured, catalog_facts);
            if let Some(h) = health_map.get(name) {
                p["healthy"] = json!(h.healthy);
                p["last_health_check_at"] = json!(h.checked_at.to_rfc3339());
                if let Some(lat) = h.latency_ms {
                    p["last_health_latency_ms"] = json!(lat);
                }
                if let Some(kind) = &h.error_kind {
                    p["last_known_error_kind"] = json!(kind);
                }
                apply_health_error(&mut p, h);
            }
            if let Some((secs, reason)) = cooldown_map.get(name) {
                let testo = reason
                    .clone()
                    .unwrap_or_else(|| testo_cooldown_predefinito(*secs));
                marca_cooldown(&mut p, *secs, Some(testo));
            }
            p
        })
        .collect()
}

/// Applica al provider JSON i dati dell'ultimo health probe canonico.
/// Il probe mcp-core (provider_health_probe.rs) e' la fonte di verita' canonica
/// per lo stato dei provider: scrive in DB, gira ogni 5 min, ha auto-recovery
/// cooldown e outage detection. Il gateway TypeScript ha un suo in-memory cache
/// che puo' restare stale (es. se loadApiKeysFromDb fallisce al boot per
/// ECONNRESET, marca tutti unhealthy senza retry). Quindi:
///   - se il probe dice healthy=true E recente (<10 min) → sovrascrive il
///     gateway: e' la verita' attuale.
///   - se il probe dice unhealthy → mantiene unhealthy (ribadiamo anche se
///     cooldown e' stato perso).
fn apply_health_probe(p: &mut serde_json::Value, h: &ProviderHealthRow) {
    p["last_health_check_at"] = json!(h.checked_at.to_rfc3339());
    if let Some(lat) = h.latency_ms {
        p["last_health_latency_ms"] = json!(lat);
    }
    if let Some(kind) = &h.error_kind {
        p["last_known_error_kind"] = json!(kind);
    }
    p["last_known_healthy"] = json!(h.healthy);
    let probe_recent = chrono::Utc::now()
        .signed_duration_since(h.checked_at)
        .num_seconds()
        < 600;
    if h.healthy && probe_recent {
        // Probe recente positivo: forza healthy=true anche se il gateway dice
        // il contrario (cache stale). Pulisce eventuale "error" stale.
        p["healthy"] = json!(true);
        if p.get("error").is_some() {
            p["error"] = json!(null);
        }
    } else if !h.healthy {
        p["healthy"] = json!(false);
        // Regola M: causa diagnosticabile di default; `apply_cooldown_or_billing`
        // (chiamato dopo, in `patch_gateway_provider`) la sovrascrive se c'e'
        // un cooldown attivo con una reason piu' specifica.
        apply_health_error(p, h);
    }
}

/// Applica cooldown attivo o billing error (mutuamente esclusivi) al provider
/// JSON. Raccoglie in `new_billing` i nuovi billing error da persistere.
fn apply_cooldown_or_billing(
    p: &mut serde_json::Value,
    name: &str,
    cooldown_map: &std::collections::HashMap<String, (u64, Option<String>)>,
    new_billing: &mut Vec<(String, String)>,
) {
    if let Some((secs, reason)) = cooldown_map.get(name) {
        let testo = reason
            .clone()
            .unwrap_or_else(|| testo_cooldown_predefinito(*secs));
        marca_cooldown(p, *secs, Some(testo));
    } else if let Some(billing_msg) = p.get("billing_error").and_then(|v| v.as_str()) {
        // Il gateway TypeScript ha rilevato un errore di billing:
        // imposta cooldown lungo e raccogliamo per la persistenza Redis.
        let billing_msg = billing_msg.to_string();
        if !crate::provider_cooldown::is_provider_in_cooldown(name) {
            crate::provider_cooldown::put_provider_in_long_cooldown(name, &billing_msg);
            tracing::warn!(
                "Provider '{}' in cooldown lungo da billing_error gateway TS: {}",
                name,
                billing_msg
            );
            new_billing.push((name.to_string(), billing_msg.clone()));
        }
        // Aggiorna il JSON di risposta per coerenza immediata
        let cooldown_duration_secs: u64 = 6 * 3600;
        marca_cooldown(p, cooldown_duration_secs, Some(billing_msg));
    }
}

/// Arricchisce un singolo provider JSON restituito dal gateway con i dati del
/// probe canonico + cooldown, e raccoglie eventuali nuovi billing error da
/// persistere. Vedi doc dei due helper per la logica di precedenza.
fn patch_gateway_provider(
    mut p: serde_json::Value,
    health_map: &std::collections::HashMap<String, ProviderHealthRow>,
    cooldown_map: &std::collections::HashMap<String, (u64, Option<String>)>,
    new_billing: &mut Vec<(String, String)>,
) -> serde_json::Value {
    let name = p["name"].as_str().unwrap_or("").to_lowercase();
    if let Some(h) = health_map.get(&name) {
        apply_health_probe(&mut p, h);
    }
    apply_cooldown_or_billing(&mut p, &name, cooldown_map, new_billing);
    p
}

/// Persiste su Redis i nuovi billing cooldown (chiave con TTL 6h + 60s).
async fn persist_billing_cooldowns(
    redis: &redis::aio::MultiplexedConnection,
    new_billing: &[(String, String)],
) {
    if new_billing.is_empty() {
        return;
    }
    let mut conn = redis.clone();
    let now_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    for (pname, pmsg) in new_billing {
        let redis_key = format!("nexus:billing_cooldown:{}", pname);
        let until_ts = now_ts.saturating_add(6 * 3600);
        let redis_value = format!("{}|{}", until_ts, pmsg);
        let _ = redis::cmd("SET")
            .arg(&redis_key)
            .arg(&redis_value)
            .arg("EX")
            .arg(6u64 * 3600 + 60)
            .query_async::<()>(&mut conn)
            .await;
    }
}

/// Arricchisce la lista provider del gateway (prima passata, sync) e persiste i
/// nuovi billing cooldown su Redis (seconda passata, async).
async fn build_patched_providers(
    state: &crate::AppState,
    body: &serde_json::Value,
    health_map: &std::collections::HashMap<String, ProviderHealthRow>,
    cooldown_map: &std::collections::HashMap<String, (u64, Option<String>)>,
) -> Vec<serde_json::Value> {
    let mut new_billing: Vec<(String, String)> = Vec::new();
    let providers_patched: Vec<serde_json::Value> = body["providers"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|p| patch_gateway_provider(p, health_map, cooldown_map, &mut new_billing))
        .collect();
    persist_billing_cooldowns(&state.redis, &new_billing).await;
    providers_patched
}

/// Le CINQUE fonti da cui si compone lo stato dei provider. Stanno insieme
/// perche' i due handler che lo espongono le vogliono tutte e cinque, e
/// caricarle a mano in due punti significa che il giorno in cui se ne aggiunge
/// una — com'e' appena successo con i fatti di catalogo — uno dei due risponde
/// con meno informazione dell'altro senza che nulla fallisca (regola L).
struct FontiStatoProvider {
    health_map: std::collections::HashMap<String, ProviderHealthRow>,
    api_key_configured: std::collections::HashMap<String, bool>,
    cooldown_map: std::collections::HashMap<String, (u64, Option<String>)>,
    provider_names: Vec<String>,
    catalog_facts: std::collections::HashMap<String, Vec<crate::provider_readiness::ModelFact>>,
}

impl FontiStatoProvider {
    async fn carica(db: &sqlx::PgPool) -> Self {
        let health_map = fetch_provider_health_map(db).await;
        let api_key_configured = fetch_api_key_configured(db).await;
        let cooldown_map = fetch_cooldown_map();
        // Dipende dalle chiavi appena lette: l'ordine non e' cosmetico.
        let provider_names = provider_names_for_status(db, &api_key_configured).await;
        let catalog_facts = crate::provider_readiness::carica_fatti_catalogo(db).await;
        Self {
            health_map,
            api_key_configured,
            cooldown_map,
            provider_names,
            catalog_facts,
        }
    }
}

pub async fn gateway_providers_handler(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let gw_url = resolve_gateway_url(&state.db).await?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let f = FontiStatoProvider::carica(&state.db).await;
    let providers_fallback = build_providers_fallback(
        &f.provider_names,
        &f.health_map,
        &f.api_key_configured,
        &f.cooldown_map,
        &f.catalog_facts,
    );
    let (health_map, cooldown_map) = (f.health_map, f.cooldown_map);

    // Nessun header di autorizzazione: `/providers` e' una rotta ESENTE nel
    // gateway (come `/health`). Prima si mandava un bearer statico, che qui non
    // serviva a nulla ed era il valore hardcoded nel sorgente.
    match client
        .get(format!("{}/providers", gw_url.trim_end_matches('/')))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().await.unwrap_or(json!({"providers": []}));
            let providers_patched =
                build_patched_providers(&state, &body, &health_map, &cooldown_map).await;
            Ok(Json(json!({
                "gateway_url": gw_url,
                "providers": providers_patched,
                "cooldown_active": cooldown_map.len(),
            })))
        }
        Ok(r) => Ok(Json(json!({
            "gateway_url": gw_url,
            "providers": providers_fallback,
            "gateway_offline": true,
            "error": format!("HTTP {}", r.status()),
            "cooldown_active": cooldown_map.len(),
        }))),
        Err(e) => Ok(Json(json!({
            "gateway_url": gw_url,
            "providers": providers_fallback,
            "gateway_offline": true,
            "error": e.to_string(),
            "cooldown_active": cooldown_map.len(),
        }))),
    }
}

pub async fn gateway_reload_handler(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let gw_url = resolve_gateway_url(&state.db).await?;
    // `/admin/reload` NON e' una rotta esente: serve una credenziale vera. E' un
    // JWT a vita breve firmato con la chiave di piattaforma, non piu' un bearer
    // statico con fallback hardcoded.
    let gw_token = nexus_auth::service_bearer(&state.db).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("bearer di servizio non disponibile: {e}") })),
        )
    })?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_default();

    match client
        .post(format!("{}/admin/reload", gw_url.trim_end_matches('/')))
        .header("Authorization", format!("Bearer {}", gw_token))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().await.unwrap_or(json!({"reloaded": true}));
            Ok(Json(body))
        }
        Ok(r) => {
            let status = r.status().as_u16();
            Err((
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": format!("Gateway returned HTTP {}", status) })),
            ))
        }
        Err(e) => Err((
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": e.to_string() })),
        )),
    }
}

// ── Embeddings: custom model validation + reindex ─────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateEmbeddingModelRequest {
    pub model: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyEmbeddingModelRequest {
    pub model: String,
    pub reindex: bool,
}

async fn upsert_setting_value(
    db: &sqlx::PgPool,
    key: &str,
    value: &str,
    category: &str,
    description: &str,
) {
    let _ = sqlx::query(
        r#"
        INSERT INTO settings (key, value, category, description, is_secret, updated_at)
        VALUES ($1, $2, $3, $4, FALSE, NOW())
        ON CONFLICT (key) DO UPDATE
        SET value = EXCLUDED.value,
            category = EXCLUDED.category,
            description = EXCLUDED.description,
            updated_at = NOW()
        "#,
    )
    .bind(key)
    .bind(value)
    .bind(category)
    .bind(description)
    .execute(db)
    .await;
    // Upsert con query propria: invalida esplicitamente, altrimenti la lettura
    // resta stantia fino alla scadenza della cache dei settings.
    nexus_auth::invalidate_setting_cache(db, key);
}

fn sanitize_collection_suffix(raw: &str) -> String {
    raw.trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

async fn probe_embedding_dimensions(_model: &str) -> Result<u64, ApiError> {
    // Embedder ONNX in-process (NexusBridge), stessa fonte di /api/embed e di
    // tutti gli embed di mcp-core (regola L). Il brain Python (REST /embed) e'
    // stato eliminato: nessuna chiamata HTTP esterna. L'embedder usa il modello
    // locale (MiniLM), quindi `_model` non seleziona un backend remoto.
    let bridge = crate::nexus_bridge::NexusBridge::global().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Embedder ONNX non inizializzato",
        )
    })?;
    let probe = "dimension probe".to_string();
    let vector = tokio::task::spawn_blocking(move || bridge.embed_one(&probe))
        .await
        .map_err(|e| {
            // Falso positivo del detector SQL: "join" qui e' il join del task
            // tokio (spawn_blocking), non una JOIN SQL. Nessuna query in questa
            // funzione. Lasciato invariato per non alterare il messaggio d'errore.
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("embed probe join: {e}"),
            )
        })?;
    let dim = vector.len() as u64;
    if dim == 0 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Impossibile determinare la dimensione embeddings",
        ));
    }
    Ok(dim)
}

pub async fn embeddings_validate_handler(
    State(_state): State<AppState>,
    Json(body): Json<ValidateEmbeddingModelRequest>,
) -> ApiResult {
    let model = body.model.trim();
    if model.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "model richiesto"));
    }
    let dim = probe_embedding_dimensions(model).await?;
    Ok(Json(
        json!({ "ok": true, "model": model, "dimensions": dim }),
    ))
}

/// Reset (drop + create) della collection Qdrant dedicata con la dimensione
/// vettoriale corretta. La delete ignora gli errori (404 se assente); la create
/// propaga un `502` se Qdrant non e' raggiungibile o rifiuta la richiesta.
async fn reindex_qdrant_collection(collection: &str, dim: u64) -> Result<(), ApiError> {
    let qdrant_url =
        crate::settings::disambigua_loopback(
            &std::env::var("QDRANT_URL").unwrap_or_else(|_| "http://localhost:6333".to_string()),
        );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_default();
    let base = qdrant_url.trim_end_matches('/');
    let delete_url = format!("{}/collections/{}", base, urlencoding::encode(collection));
    let _ = client.delete(&delete_url).send().await; // ignore errors (404 etc.)

    let create_url = format!("{}/collections/{}", base, urlencoding::encode(collection));
    let create_body = json!({ "vectors": { "size": dim, "distance": "Cosine" } });
    let resp = client
        .put(&create_url)
        .json(&create_body)
        .send()
        .await
        .map_err(|e| {
            api_error(
                StatusCode::BAD_GATEWAY,
                format!("Qdrant non raggiungibile: {e}"),
            )
        })?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(api_error(
            StatusCode::BAD_GATEWAY,
            format!("Qdrant create collection fallito (HTTP {status}): {text}"),
        ));
    }
    Ok(())
}

pub async fn embeddings_apply_handler(
    State(state): State<AppState>,
    Json(body): Json<ApplyEmbeddingModelRequest>,
) -> ApiResult {
    let model = body.model.trim();
    if model.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "model richiesto"));
    }

    let dim = probe_embedding_dimensions(model).await?;
    let suffix = sanitize_collection_suffix(&format!("{}-{}", dim, model));
    let collection = format!("code_embeddings_{}", suffix);

    // Reindex = reset della collection dedicata (drop/create) con dimensione corretta.
    if body.reindex {
        reindex_qdrant_collection(&collection, dim).await?;
    }

    upsert_setting_value(
        &state.db,
        "embedding_model",
        model,
        "embeddings",
        "Sentence-transformers model",
    )
    .await;
    upsert_setting_value(
        &state.db,
        "qdrant_collection",
        &collection,
        "infrastructure",
        "Qdrant collection name",
    )
    .await;

    Ok(Json(json!({
        "ok": true,
        "model": model,
        "dimensions": dim,
        "qdrantCollection": collection,
        "reindexed": body.reindex
    })))
}

// ── Admin: gestione cooldown provider ────────────────────────────────────────

/// GET /api/internal/providers/status
///
/// Endpoint no-auth dedicato al nexus-gateway TypeScript per leggere lo stato
/// canonico dei provider senza tenere una sua cache in-memory (che andava
/// stale e creava inconsistenza tra le due "verità" mcp-core vs gateway).
///
/// Ritorna per ogni provider noto:
///   - `healthy`: true/false dal probe Rust (provider_health_probe.rs)
///   - `last_check`: ISO timestamp dell'ultimo probe
///   - `latency_ms`: latency del probe (se disponibile)
///   - `error_kind`: tipo errore se unhealthy (quota_exceeded, billing_required, ecc.)
///   - `error`: messaggio diagnostico se unhealthy (regola M: mai un
///     `healthy=false` senza causa leggibile) — dal probe se non c'e' un
///     cooldown mcp-core attivo, altrimenti dalla reason del cooldown.
///   - `cooldown_seconds_remaining`: se in cooldown attivo
///   - `configured`: se la API key è presente in `settings`
///
/// Differenza con `gateway_providers_handler`: questo NON chiama il gateway
/// (evita loop), e NON richiede auth (è solo lettura aggregata pubblica).
pub async fn providers_status_internal(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
) -> Json<Value> {
    // Fonti canoniche riusate (regola L): health probe DB, API key in settings,
    // cooldown snapshot in-process (`provider_cooldown.rs`, stato applicato da
    // mcp-core quando classify provider_error riconosce billing/rate_limit/
    // overloaded). Lo stesso dato e' esposto da
    // `/api/neural/providers/billing-cooldown`.
    let f = FontiStatoProvider::carica(&state.db).await;

    let providers: Vec<Value> = f
        .provider_names
        .iter()
        .map(|name| {
            build_provider_status_entry(
                name,
                &f.health_map,
                &f.cooldown_map,
                &f.api_key_configured,
                &f.catalog_facts,
            )
        })
        .collect();

    Json(json!({ "providers": providers }))
}

/// Costruisce l'entry JSON di un singolo provider per `/api/internal/providers/status`.
/// Punto unico (regola L), estratto per essere testabile senza DB/AppState
/// (regola O: la funzione pura e' la stessa che serve la route, non una sua
/// imitazione nel test).
fn build_provider_status_entry(
    name: &str,
    health_map: &std::collections::HashMap<String, ProviderHealthRow>,
    cooldown_map: &std::collections::HashMap<String, (u64, Option<String>)>,
    api_key_configured: &std::collections::HashMap<String, bool>,
    catalog_facts: &std::collections::HashMap<String, Vec<crate::provider_readiness::ModelFact>>,
) -> Value {
    let mut p = entry_con_prontezza(name, health_map, api_key_configured, catalog_facts);
    if let Some(h) = health_map.get(name) {
        p["healthy"] = json!(h.healthy);
        p["last_check"] = json!(h.checked_at.to_rfc3339());
        if let Some(lat) = h.latency_ms {
            p["latency_ms"] = json!(lat);
        }
        if let Some(kind) = &h.error_kind {
            p["error_kind"] = json!(kind);
        }
        // Regola M: un healthy=false senza causa non e' diagnosticabile.
        // Il messaggio pieno (gia' persistito, mig 0097) e' il default;
        // il cooldown_map sotto lo sovrascrive con la reason attiva se
        // piu' recente/specifica.
        apply_health_error(&mut p, h);
    }
    if let Some((secs, reason)) = cooldown_map.get(name) {
        marca_cooldown(&mut p, *secs, reason.clone());
    }
    p
}

/// Entry di base di un provider: identita', `configured` e la PRONTEZZA.
///
/// Punto unico delle due rese dello stato provider (regola L). Divergono dopo
/// — quella interna usa `last_check`/`latency_ms`/`error_kind`, quella di
/// ripiego `last_health_*`/`last_known_*` — ma partono dagli stessi tre fatti,
/// e tenerne due copie e' il modo in cui, aggiungendo la prontezza, una delle
/// due sarebbe potuta restare indietro senza che nulla fallisse.
///
/// `healthy: null` da solo non dice PERCHE' non si sa nulla: la prontezza lo
/// dichiara in un campo (regola Q), delegando al punto unico che interroga i
/// due cicli di verifica reali.
///
/// Accanto alla prontezza viaggia la COPERTURA DELLA DICHIARAZIONE, che risponde
/// a un'altra domanda sugli stessi fatti di catalogo: la prima dice se sappiamo
/// che il fornitore risponde, la seconda se cio' che sappiamo basta a usarlo. Le
/// due non si possono fondere in un campo solo — un fornitore sano e interamente
/// privo di capability e' il caso REALE di groq e openrouter (misurato il
/// 10/08/2026), e collassarle perderebbe l'una o l'altra meta'.
fn entry_con_prontezza(
    name: &str,
    health_map: &std::collections::HashMap<String, ProviderHealthRow>,
    api_key_configured: &std::collections::HashMap<String, bool>,
    catalog_facts: &std::collections::HashMap<String, Vec<crate::provider_readiness::ModelFact>>,
) -> Value {
    let configured = api_key_configured.get(name).copied().unwrap_or(false);
    let modelli = catalog_facts.get(name).map(Vec::as_slice).unwrap_or(&[]);
    let mut p = json!({
        "name": name,
        "configured": configured,
        "healthy": serde_json::Value::Null,
    });
    crate::provider_readiness::scrivi_prontezza(
        &mut p,
        &crate::provider_readiness::classifica(
            configured,
            modelli,
            health_map.get(name).map(|h| h.healthy),
        ),
    );
    crate::provider_declaration::scrivi_dichiarazione(
        &mut p,
        &crate::provider_declaration::classifica_dichiarazione(modelli),
    );
    p
}

/// Marca l'entry come non disponibile per un cooldown attivo. Punto unico dei
/// TRE call site (regola L): `healthy` e `readiness` raccontano lo stesso
/// fornitore e non possono contraddirsi nella stessa risposta — un cooldown E'
/// un'osservazione — e prima l'unico modo di tenerli allineati era ricordarsene
/// in ogni ramo. `error` resta opzionale perche' la resa interna lo omette
/// quando il cooldown non porta una reason.
fn marca_cooldown(p: &mut Value, secs: u64, error: Option<String>) {
    p["healthy"] = json!(false);
    crate::provider_readiness::scrivi_prontezza(
        p,
        &crate::provider_readiness::ProviderReadiness::Observed { healthy: false },
    );
    p["cooldown_seconds_remaining"] = json!(secs);
    if let Some(e) = error {
        p["error"] = json!(e);
    }
}

/// Testo di ripiego quando il cooldown non porta una reason propria.
fn testo_cooldown_predefinito(secs: u64) -> String {
    format!("In cooldown ({secs}s rimanenti) — l'AI userà un altro provider")
}

/// POST /api/admin/routing-matrix/auto-promote-now
/// Forza un round del routing_matrix_auto_promoter (per test e admin)
pub async fn admin_routing_matrix_auto_promote_now(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
) -> Json<Value> {
    match crate::routing_matrix_auto_promoter::run_one_round(&state.db).await {
        Ok(stats) => Json(json!({
            "ok": true,
            "updated": stats.updated,
            "skipped_manual": stats.skipped_manual,
            "no_candidates": stats.no_candidates,
        })),
        Err(e) => Json(json!({ "ok": false, "error": e.to_string() })),
    }
}

/// Riga della vista budget (a livello modulo per `merge_budget_entries` e i test).
/// Casto NUMERIC -> TEXT in SQL per evitare la dipendenza rust_decimal:
/// sqlx mappa NUMERIC::text -> String senza problemi.
#[derive(sqlx::FromRow)]
struct BudgetRow {
    provider: String,
    monthly_budget_usd: String,
    spent_current_period_usd: String,
    remaining_usd: String,
    min_threshold_usd: String,
    is_exhausted: bool,
    period_start: chrono::DateTime<chrono::Utc>,
}

/// Unione registry-aware delle righe budget (punto unico, regola L; pura: testabile
/// senza DB). Ogni provider attivo (`names`, da `provider_names_for_status`) appare:
/// con la sua riga reale (`configured: true`) oppure con una entry sintetica a budget
/// zero (`configured: false`) se la tabella non ha ancora la riga — cosi' i provider
/// onboardati via registry (mig 0565+) sono gestibili dal pannello senza seed SQL
/// (set-budget fa UPSERT e crea la riga al primo "Imposta budget"). Le righe orfane
/// (provider con budget ma rimosso dalle fonti) restano visibili in coda.
fn merge_budget_entries(names: &[String], rows: Vec<BudgetRow>) -> Vec<Value> {
    let now = chrono::Utc::now();
    let mut by_provider: std::collections::BTreeMap<String, BudgetRow> =
        rows.into_iter().map(|r| (r.provider.clone(), r)).collect();
    let mut items: Vec<Value> = Vec::with_capacity(names.len());
    for name in names {
        match by_provider.remove(name) {
            Some(r) => items.push(json!({
                "provider": r.provider,
                "monthly_budget_usd": r.monthly_budget_usd,
                "spent_usd": r.spent_current_period_usd,
                "remaining_usd": r.remaining_usd,
                "min_threshold_usd": r.min_threshold_usd,
                "is_exhausted": r.is_exhausted,
                "period_start": r.period_start.to_rfc3339(),
                "configured": true,
            })),
            None => items.push(json!({
                "provider": name,
                "monthly_budget_usd": "0",
                "spent_usd": "0",
                "remaining_usd": "0",
                // Default allineato a admin_set_provider_budget (threshold 1.0).
                "min_threshold_usd": "1.0",
                "is_exhausted": false,
                "period_start": now.to_rfc3339(),
                "configured": false,
            })),
        }
    }
    // Righe orfane: budget impostato per un provider non piu' nelle fonti.
    for (_, r) in by_provider {
        items.push(json!({
            "provider": r.provider,
            "monthly_budget_usd": r.monthly_budget_usd,
            "spent_usd": r.spent_current_period_usd,
            "remaining_usd": r.remaining_usd,
            "min_threshold_usd": r.min_threshold_usd,
            "is_exhausted": r.is_exhausted,
            "period_start": r.period_start.to_rfc3339(),
            "configured": true,
        }));
    }
    items
}

/// GET /api/admin/providers/budget
/// Ritorna il budget residuo per ogni provider attivo (registry-aware): i provider
/// senza riga budget compaiono come "non impostato" invece di sparire dal pannello.
pub async fn admin_providers_budget_list(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
) -> Json<Value> {
    let rows: Vec<BudgetRow> = sqlx::query_as::<_, BudgetRow>(
        "SELECT provider,
                monthly_budget_usd::text AS monthly_budget_usd,
                spent_current_period_usd::text AS spent_current_period_usd,
                remaining_usd::text AS remaining_usd,
                min_threshold_usd::text AS min_threshold_usd,
                is_exhausted,
                period_start
           FROM provider_budget_remaining_view
          ORDER BY provider",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    // Lista provider dal punto unico registry-aware (catalog + registry + api_key).
    let api_key_configured = fetch_api_key_configured(&state.db).await;
    let names = provider_names_for_status(&state.db, &api_key_configured).await;
    Json(json!({ "providers": merge_budget_entries(&names, rows) }))
}

#[derive(serde::Deserialize)]
pub struct SetBudgetBody {
    pub monthly_budget_usd: f64,
    #[serde(default)]
    pub min_threshold_usd: Option<f64>,
}

/// POST /api/admin/providers/:name/set-budget
/// Imposta il budget mensile (e opzionalmente soglia minima) per un provider.
/// Tipicamente chiamato quando l'admin ricarica l'account presso il provider.
pub async fn admin_set_provider_budget(
    axum::extract::Path(name): axum::extract::Path<String>,
    axum::extract::State(state): axum::extract::State<crate::AppState>,
    Json(body): Json<SetBudgetBody>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    if body.monthly_budget_usd < 0.0 {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": "monthly_budget_usd deve essere >= 0"})),
        ));
    }
    let provider = name.to_lowercase();
    let threshold = body.min_threshold_usd.unwrap_or(1.0);
    let _ = sqlx::query(
        "INSERT INTO provider_budget_status
            (provider, monthly_budget_usd, min_threshold_usd, period_start, spent_current_period_usd)
         VALUES ($1, $2, $3, NOW(), 0)
         ON CONFLICT (provider) DO UPDATE
            SET monthly_budget_usd = EXCLUDED.monthly_budget_usd,
                min_threshold_usd = EXCLUDED.min_threshold_usd,
                updated_at = NOW()",
    )
    .bind(&provider)
    .bind(body.monthly_budget_usd)
    .bind(threshold)
    .execute(&state.db)
    .await
    .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;
    Ok(Json(
        json!({"ok": true, "provider": provider, "monthly_budget_usd": body.monthly_budget_usd}),
    ))
}

/// POST /api/admin/providers/:name/recharge-budget
/// Azzera lo `spent` e ricomincia il periodo. Da chiamare dopo che l'admin
/// ha effettivamente ricaricato l'account presso il provider.
pub async fn admin_recharge_provider_budget(
    axum::extract::Path(name): axum::extract::Path<String>,
    axum::extract::State(state): axum::extract::State<crate::AppState>,
) -> Json<Value> {
    let provider = name.to_lowercase();
    let _ = sqlx::query(
        "UPDATE provider_budget_status
            SET spent_current_period_usd = 0,
                period_start = NOW(),
                updated_at = NOW()
          WHERE provider = $1",
    )
    .bind(&provider)
    .execute(&state.db)
    .await;
    // Rimuovi anche eventuale cooldown billing (l'utente ha ricaricato).
    crate::provider_cooldown::remove_cooldown(&provider);
    Json(json!({"ok": true, "provider": provider}))
}

/// GET /api/admin/providers/cooldown
/// Restituisce la lista di tutti i provider attualmente in cooldown.
pub async fn admin_cooldown_list() -> Json<Value> {
    let snapshot = crate::provider_cooldown::cooldown_snapshot();
    let items: Vec<Value> = snapshot
        .into_iter()
        .map(|(name, secs, reason)| {
            json!({
                "provider": name,
                "remaining_seconds": secs,
                "reason": reason,
            })
        })
        .collect();
    Json(json!({ "cooldowns": items }))
}

/// POST /api/admin/providers/:name/reset-cooldown
/// Rimuove il cooldown di un provider, permettendogli di tornare
/// immediatamente in servizio. Rimuove anche il contatore failures
/// del circuit breaker e la persistenza Redis.
pub async fn admin_reset_provider_cooldown(
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Json<Value> {
    crate::provider_cooldown::remove_cooldown(&name);
    Json(json!({
        "ok": true,
        "provider": name.to_lowercase(),
        "message": format!("Cooldown rimosso per '{}'", name)
    }))
}
