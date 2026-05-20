use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::process::Command;
use tokio::time::timeout;
use std::time::Duration;

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
        Self { id: id.into(), label: label.into(), status: "ok".into(), detail: detail.into() }
    }
    fn warn(id: &str, label: &str, detail: impl Into<String>) -> Self {
        Self { id: id.into(), label: label.into(), status: "warn".into(), detail: detail.into() }
    }
    fn error(id: &str, label: &str, detail: impl Into<String>) -> Self {
        Self { id: id.into(), label: label.into(), status: "error".into(), detail: detail.into() }
    }
}

async fn check_db(db: &sqlx::PgPool) -> EnvironmentCheck {
    match sqlx::query("SELECT 1").fetch_one(db).await {
        Ok(_) => EnvironmentCheck::ok("db", "PostgreSQL", "Connected"),
        Err(e) => EnvironmentCheck::error("db", "PostgreSQL", format!("{e}")),
    }
}

async fn check_playwright_libs() -> EnvironmentCheck {
    // Prova ldconfig -p
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
        EnvironmentCheck::ok("playwright_libs", "Playwright system libs", "libatk-1.0.so.0 found")
    } else {
        EnvironmentCheck::error("playwright_libs", "Playwright system libs", "libatk-1.0.so.0 missing")
    }
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

/// Verifica che il gRPC ToolRunner sia in ascolto su 127.0.0.1:50071.
/// Senza questo gRPC il brain Python non può invocare i tool MCP (read_file,
/// str_replace, ecc.) e l'AI fallisce con "0 step" o "tool gRPC unreachable".
async fn check_tool_runner() -> EnvironmentCheck {
    let addr = std::env::var("TOOL_RUNNER_ADDR").unwrap_or_else(|_| "127.0.0.1:50071".into());
    let host_port: Vec<&str> = addr.split(':').collect();
    let port = host_port.get(1).and_then(|p| p.parse::<u16>().ok()).unwrap_or(50071);
    // Tentativo di TCP connect non bloccante (timeout 1s)
    let connect = tokio::time::timeout(
        Duration::from_millis(1000),
        tokio::net::TcpStream::connect(format!("127.0.0.1:{}", port)),
    )
    .await;
    match connect {
        Ok(Ok(_)) => EnvironmentCheck::ok("tool_runner", "MCP Tools (gRPC)", format!("listening on :{}", port)),
        Ok(Err(_)) | Err(_) => EnvironmentCheck::error(
            "tool_runner",
            "MCP Tools (gRPC)",
            format!("port :{} not reachable — l'AI non potrà usare i tool", port),
        ),
    }
}

/// Controlla i 5 microservizi Rust ausiliari (admin, chat, doc, billing, plugin).
/// Li verifica in parallelo con TCP connect (1s timeout); restituisce un check
/// aggregato con il dettaglio per ciascun servizio.
async fn check_microservices() -> EnvironmentCheck {
    let services = [
        ("admin-service",   4010u16),
        ("chat-service",    4020),
        ("doc-service",     4030),
        ("billing-service", 4040),
        ("plugin-service",  4050),
    ];

    let mut results: Vec<(&str, bool)> = Vec::with_capacity(services.len());
    let handles: Vec<_> = services.iter().map(|(name, port)| {
        let addr = format!("127.0.0.1:{port}");
        let p = *port;
        (*name, tokio::spawn(async move {
            timeout(
                Duration::from_secs(1),
                tokio::net::TcpStream::connect(format!("127.0.0.1:{p}")),
            )
            .await
            .map(|r| r.is_ok())
            .unwrap_or(false)
        }))
    }).collect();

    for (name, handle) in handles {
        let ok = handle.await.unwrap_or(false);
        results.push((name, ok));
    }

    let ok_count = results.iter().filter(|(_, ok)| *ok).count();
    let total = results.len();
    let detail = results.iter()
        .map(|(name, ok)| format!("{}: {}", name, if *ok { "ok" } else { "down" }))
        .collect::<Vec<_>>()
        .join(", ");

    if ok_count == total {
        EnvironmentCheck::ok("microservices", "Microservizi (admin/chat/doc/billing/plugin)", format!("{ok_count}/{total} operativi"))
    } else if ok_count > 0 {
        EnvironmentCheck::warn("microservices", "Microservizi (admin/chat/doc/billing/plugin)", format!("{ok_count}/{total} operativi — {detail}"))
    } else {
        EnvironmentCheck::error("microservices", "Microservizi (admin/chat/doc/billing/plugin)", format!("0/{total} operativi — {detail}"))
    }
}

async fn check_brain_service() -> EnvironmentCheck {
    let result = Command::new("pgrep")
        .args(["-f", "brain.grpc_server.main|nexus-brain|uvicorn"])
        .output()
        .await;

    match result {
        Ok(out) if !out.stdout.is_empty() => {
            let pid = String::from_utf8_lossy(&out.stdout).trim().replace('\n', ",");
            EnvironmentCheck::ok("brain_service", "Nexus Brain (Python AI)", format!("pid {pid}"))
        }
        _ => EnvironmentCheck::error("brain_service", "Nexus Brain (Python AI)", "not running"),
    }
}

fn check_backend_process() -> EnvironmentCheck {
    let pid = std::process::id();
    EnvironmentCheck::ok("backend_process", "Backend mcp-core", format!("pid {pid}"))
}

async fn check_frontend_process() -> EnvironmentCheck {
    let result = Command::new("ss").args(["-tlnp"]).output().await;
    match result {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            if stdout.contains(":3000") {
                EnvironmentCheck::ok("frontend_process", "Frontend web-ide", "Port 3000 listening")
            } else {
                EnvironmentCheck::error("frontend_process", "Frontend web-ide", "Port 3000 not listening")
            }
        }
        Err(e) => EnvironmentCheck::warn("frontend_process", "Frontend web-ide", format!("ss failed: {e}")),
    }
}

async fn check_migrations(db_url: &str) -> EnvironmentCheck {
    // Risolve il path di sqlx-cli: prima il path esplicito, poi cerca nel PATH via `which`.
    let sqlx_path = if std::path::Path::new("/home/administrator/.cargo/bin/sqlx").exists() {
        "/home/administrator/.cargo/bin/sqlx".to_string()
    } else {
        let which_out = Command::new("which").arg("sqlx").output().await;
        match which_out {
            Ok(o) if o.status.success() => {
                String::from_utf8_lossy(&o.stdout).trim().to_string()
            }
            _ => {
                // sqlx-cli non installato: id dedicato per mostrare il pulsante di installazione
                return EnvironmentCheck::warn(
                    "migrations_sqlx_missing",
                    "DB Migrations",
                    "sqlx-cli non installato",
                );
            }
        }
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
            let pending: usize = stdout.lines()
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
        EnvironmentCheck::ok("ai_providers", "AI Providers", format!("{count} providers configured"))
    } else {
        EnvironmentCheck::warn("ai_providers", "AI Providers", "0 providers configured")
    }
}

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

pub async fn get_environment_status(
    State(state): State<AppState>,
) -> ApiResult {
    let db_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://nexus:nexus@localhost:5433/nexus".to_string());

    let (
        db_check,
        playwright_libs_check,
        playwright_browser_check,
        brain_check,
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
        check_brain_service(),
        check_tool_runner(),
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
        brain_check,
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

pub async fn fix_environment(
    State(_state): State<AppState>,
    Json(body): Json<FixRequest>,
) -> ApiResult {
    match body.action.as_str() {
        "install_playwright_browsers" => {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
            let result = timeout(
                Duration::from_secs(120),
                Command::new("npx")
                    .args(["playwright", "install", "chromium"])
                    .current_dir(&home)
                    .output(),
            )
            .await;

            match result {
                Ok(Ok(out)) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let output = format!("{stdout}{stderr}");
                    Ok(Json(json!({ "ok": out.status.success(), "output": output })))
                }
                Ok(Err(e)) => Ok(Json(json!({ "ok": false, "output": format!("Error: {e}") }))),
                Err(_) => Ok(Json(json!({ "ok": false, "output": "Timeout after 120s" }))),
            }
        }

        "run_migrations" => {
            let db_url = std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgres://nexus:nexus@localhost:5433/nexus".to_string());
            let result = timeout(
                Duration::from_secs(60),
                Command::new("/home/administrator/.cargo/bin/sqlx")
                    .args(["migrate", "run", "--database-url", &db_url])
                    .output(),
            )
            .await;

            match result {
                Ok(Ok(out)) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let output = format!("{stdout}{stderr}");
                    Ok(Json(json!({ "ok": out.status.success(), "output": output })))
                }
                Ok(Err(e)) => Ok(Json(json!({ "ok": false, "output": format!("Error: {e}") }))),
                Err(_) => Ok(Json(json!({ "ok": false, "output": "Timeout after 60s" }))),
            }
        }

        "get_system_deps_command" => {
            Ok(Json(json!({
                "ok": true,
                "output": "sudo apt-get install -y libatk1.0-0 libatk-bridge2.0-0 libcups2 libxcomposite1 libxdamage1 libxfixes3 libxrandr2 libgbm1 libpango-1.0-0 libcairo2 libasound2t64 libnspr4 libnss3 libx11-xcb1 libxcb-dri3-0 libdrm2 libglib2.0-0"
            })))
        }

        "restart_frontend" => {
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
                .args(["-c", &format!("cd {frontend_dir} && nohup pnpm start > /tmp/web-ide.log 2>&1 &")])
                .output()
                .await;

            match result {
                Ok(_) => Ok(Json(json!({ "ok": true, "output": "Frontend restart initiated. Check port 3000 in a few seconds." }))),
                Err(e) => Ok(Json(json!({ "ok": false, "output": format!("Error: {e}") }))),
            }
        }

        "install_system_deps" => {
            let sudo_password = body.sudo_password.as_deref().unwrap_or("");
            if sudo_password.is_empty() {
                return Err(api_error(StatusCode::BAD_REQUEST, "sudo_password required"));
            }

            let packages = "libatk1.0-0 libatk-bridge2.0-0 libcups2 libxcomposite1 libxdamage1 libxfixes3 libxrandr2 libgbm1 libpango-1.0-0 libcairo2 libasound2t64 libnspr4 libnss3 libx11-xcb1 libxcb-dri3-0 libdrm2 libglib2.0-0 libdbus-1-3 libxshmfence1 libxext6";

            let cmd = format!("echo '{}' | sudo -S apt-get install -y {} 2>&1", sudo_password, packages);

            let result = tokio::time::timeout(
                Duration::from_secs(120),
                tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd)
                    .output()
            ).await;

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

        "install_sqlx_cli" => {
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

            match result {
                Ok(Ok(out)) => {
                    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                    let output = format!("{stdout}{stderr}");
                    Ok(Json(json!({ "ok": out.status.success(), "output": output })))
                }
                Ok(Err(e)) => Ok(Json(json!({ "ok": false, "output": format!("Errore avvio cargo: {e}") }))),
                Err(_) => Ok(Json(json!({ "ok": false, "output": "Timeout dopo 300s. Riprova." }))),
            }
        }

        _ => Err(api_error(StatusCode::BAD_REQUEST, format!("Unknown action: {}", body.action))),
    }
}

pub async fn qdrant_health_handler(
    axum::extract::State(_state): axum::extract::State<crate::AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let qdrant_url = std::env::var("QDRANT_URL").unwrap_or_else(|_| "http://localhost:6333".to_string());
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
                Ok(v) => v["result"]["collections"].as_array().map(|a| a.len()).unwrap_or(0),
                Err(_) => 0,
            },
            Err(_) => 0,
        }
    } else { 0 };

    let mut result = json!({ "healthy": healthy, "url": qdrant_url, "collections": collections });
    if let Some(err) = error {
        result["error"] = serde_json::Value::String(err);
    }
    Ok(Json(result))
}

pub async fn gateway_providers_handler(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let gw_url = std::env::var("NEXUS_GATEWAY_URL").unwrap_or_else(|_| "http://localhost:4060".to_string());
    let gw_token = std::env::var("NEXUS_GATEWAY_SERVICE_TOKEN").unwrap_or_else(|_| "dev-internal-token".to_string());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    // Snapshot dei provider in cooldown (in-memory) per riportarli come unhealthy
    // anche se il gateway li riporta come "configurati".
    let cooldown_map: std::collections::HashMap<String, (u64, Option<String>)> =
        crate::provider_cooldown::cooldown_snapshot()
            .into_iter()
            .map(|(name, secs, reason)| (name, (secs, reason)))
            .collect();

    // Ultimo health check per provider (popolato dal worker provider_health_probe).
    // Permette al frontend di mostrare timestamp ultimo ping + ultima latency,
    // anche prima che il primo errore reale dell'utente arrivi.
    // DISTINCT ON e' un'estensione PostgreSQL: prende per ogni provider la riga
    // piu' recente in O(N log N) sull'indice (provider, checked_at DESC).
    #[derive(sqlx::FromRow)]
    struct HealthRow {
        provider: String,
        healthy: bool,
        latency_ms: Option<i32>,
        error_kind: Option<String>,
        checked_at: chrono::DateTime<chrono::Utc>,
    }
    let health_rows: Vec<HealthRow> = sqlx::query_as::<_, HealthRow>(
        r#"SELECT DISTINCT ON (provider)
                  provider, healthy, latency_ms, error_kind, checked_at
           FROM nexus_provider_health_history
           ORDER BY provider, checked_at DESC"#,
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let health_map: std::collections::HashMap<String, HealthRow> = health_rows
        .into_iter()
        .map(|r| (r.provider.clone(), r))
        .collect();

    // Query presenza API key per provider (usata nel fallback se gateway offline).
    // Mappa: "anthropic" -> true se anthropic_api_key non è vuoto.
    #[derive(sqlx::FromRow)]
    struct SettingsRow {
        key: String,
        value: String,
    }
    let settings_rows: Vec<SettingsRow> = sqlx::query_as::<_, SettingsRow>(
        "SELECT key, value FROM settings WHERE category = 'providers' AND key LIKE '%_api_key'",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let api_key_configured: std::collections::HashMap<String, bool> = settings_rows
        .into_iter()
        .map(|r| {
            let provider = r.key.trim_end_matches("_api_key").to_string();
            (provider, !r.value.trim().is_empty())
        })
        .collect();

    // Lista fallback costruita da health_map + api_key_configured + cooldown_map.
    // Usata quando il gateway TypeScript (4060) non è raggiungibile, così i LED
    // mostrano l'ultimo stato noto invece di essere tutti grigi.
    const KNOWN_PROVIDERS_LIST: [&str; 5] = ["anthropic", "openai", "google", "deepseek", "mistral"];
    let providers_fallback: Vec<serde_json::Value> = KNOWN_PROVIDERS_LIST.iter().map(|&name| {
        let configured = api_key_configured.get(name).copied().unwrap_or(false);
        let mut p = json!({
            "name": name,
            "configured": configured,
            "healthy": serde_json::Value::Null,
        });
        if let Some(h) = health_map.get(name) {
            p["healthy"] = json!(h.healthy);
            p["last_health_check_at"] = json!(h.checked_at.to_rfc3339());
            if let Some(lat) = h.latency_ms {
                p["last_health_latency_ms"] = json!(lat);
            }
            if let Some(kind) = &h.error_kind {
                p["last_known_error_kind"] = json!(kind);
            }
        }
        if let Some((secs, reason)) = cooldown_map.get(name) {
            p["healthy"] = json!(false);
            p["cooldown_seconds_remaining"] = json!(secs);
            p["error"] = json!(reason.clone().unwrap_or_else(||
                format!("In cooldown ({}s rimanenti) — l'AI userà un altro provider", secs)
            ));
        }
        p
    }).collect();

    match client
        .get(format!("{}/providers", gw_url.trim_end_matches('/')))
        .header("Authorization", format!("Bearer {}", gw_token))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().await.unwrap_or(json!({"providers": []}));
            // Prima passata: raccogli i nuovi billing errors (non async, dentro closure).
            let mut new_billing: Vec<(String, String)> = Vec::new();
            let providers_patched: Vec<serde_json::Value> = body["providers"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|mut p| {
                    let name = p["name"].as_str().unwrap_or("").to_lowercase();
                    // Arricchimento con dati health probe (se disponibili).
                    // Il probe mcp-core (provider_health_probe.rs) e' la fonte di
                    // verita' canonica per lo stato dei provider: scrive in DB,
                    // gira ogni 5 min, ha auto-recovery cooldown e outage detection.
                    // Il gateway TypeScript ha un suo in-memory cache che puo'
                    // restare stale (es. se loadApiKeysFromDb fallisce al boot
                    // per ECONNRESET, marca tutti unhealthy senza retry). Quindi:
                    //   - se il probe dice healthy=true E recente (<10 min) →
                    //     sovrascrive il gateway: e' la verita' attuale.
                    //   - se il probe dice unhealthy → mantiene unhealthy
                    //     (ribadiamo anche se cooldown e' stato perso).
                    if let Some(h) = health_map.get(&name) {
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
                            .num_seconds() < 600;
                        if h.healthy && probe_recent {
                            // Probe recente positivo: forza healthy=true
                            // anche se il gateway dice il contrario (cache stale).
                            p["healthy"] = json!(true);
                            // Pulisce eventuale "error" stale dal gateway.
                            if p.get("error").is_some() {
                                p["error"] = json!(null);
                            }
                        } else if !h.healthy {
                            p["healthy"] = json!(false);
                        }
                    }
                    if let Some((secs, reason)) = cooldown_map.get(&name) {
                        p["healthy"] = json!(false);
                        p["cooldown_seconds_remaining"] = json!(secs);
                        p["error"] = json!(reason.clone().unwrap_or_else(||
                            format!("In cooldown ({}s rimanenti) — l'AI userà un altro provider", secs)
                        ));
                    } else if let Some(billing_msg) = p.get("billing_error").and_then(|v| v.as_str()) {
                        // Il gateway TypeScript ha rilevato un errore di billing:
                        // imposta cooldown lungo e raccogliamo per la persistenza Redis.
                        let billing_msg = billing_msg.to_string();
                        if !crate::provider_cooldown::is_provider_in_cooldown(&name) {
                            crate::provider_cooldown::put_provider_in_long_cooldown(&name, &billing_msg);
                            tracing::warn!(
                                "Provider '{}' in cooldown lungo da billing_error gateway TS: {}",
                                name, billing_msg
                            );
                            new_billing.push((name.clone(), billing_msg.clone()));
                        }
                        // Aggiorna il JSON di risposta per coerenza immediata
                        let cooldown_duration_secs: u64 = 6 * 3600;
                        p["healthy"] = json!(false);
                        p["cooldown_seconds_remaining"] = json!(cooldown_duration_secs);
                        p["error"] = json!(billing_msg);
                    }
                    p
                })
                .collect();
            // Seconda passata (async): persisti i nuovi billing errors su Redis.
            if !new_billing.is_empty() {
                let mut conn = state.redis.clone();
                let now_ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                for (pname, pmsg) in &new_billing {
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
    axum::extract::State(_state): axum::extract::State<crate::AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let gw_url = std::env::var("NEXUS_GATEWAY_URL").unwrap_or_else(|_| "http://localhost:4060".to_string());
    let gw_token = std::env::var("NEXUS_GATEWAY_SERVICE_TOKEN").unwrap_or_else(|_| "dev-internal-token".to_string());
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
            // Reload Brain settings too (disables/enables providers based on DB flags)
            let neural_url = std::env::var("BRAIN_REST_URL").unwrap_or_else(|_| "http://localhost:8001".to_string());
            let _ = client
                .post(format!("{}/reload-settings", neural_url.trim_end_matches('/')))
                .json(&serde_json::json!({}))
                .send().await;
            Ok(Json(body))
        }
        Ok(r) => {
            let status = r.status().as_u16();
            Err((StatusCode::BAD_GATEWAY, Json(json!({ "error": format!("Gateway returned HTTP {}", status) }))))
        }
        Err(e) => Err((StatusCode::BAD_GATEWAY, Json(json!({ "error": e.to_string() })))),
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

async fn upsert_setting_value(db: &sqlx::PgPool, key: &str, value: &str, category: &str, description: &str) {
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
}

fn sanitize_collection_suffix(raw: &str) -> String {
    raw.trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

async fn probe_embedding_dimensions(model: &str) -> Result<u64, ApiError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .build()
        .unwrap_or_default();
    let neural_url =
        std::env::var("BRAIN_REST_URL").unwrap_or_else(|_| "http://localhost:8001".to_string());
    let resp = client
        .post(format!("{}/embed", neural_url.trim_end_matches('/')))
        .json(&json!({ "model": model, "text": "dimension probe", "texts": [] }))
        .send()
        .await
        .map_err(|e| api_error(StatusCode::BAD_GATEWAY, format!("Neural Core non raggiungibile: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            format!("Validazione fallita (HTTP {status}): {text}"),
        ));
    }

    let payload: serde_json::Value = resp.json().await.unwrap_or(json!({}));
    let dim = payload.get("dimensions").and_then(|v| v.as_u64()).unwrap_or(0);
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
    Ok(Json(json!({ "ok": true, "model": model, "dimensions": dim })))
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
        let qdrant_url =
            std::env::var("QDRANT_URL").unwrap_or_else(|_| "http://localhost:6333".to_string());
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        let base = qdrant_url.trim_end_matches('/');
        let delete_url = format!("{}/collections/{}", base, urlencoding::encode(&collection));
        let _ = client.delete(&delete_url).send().await; // ignore errors (404 etc.)

        let create_url = format!("{}/collections/{}", base, urlencoding::encode(&collection));
        let create_body = json!({ "vectors": { "size": dim, "distance": "Cosine" } });
        let resp = client
            .put(&create_url)
            .json(&create_body)
            .send()
            .await
            .map_err(|e| api_error(StatusCode::BAD_GATEWAY, format!("Qdrant non raggiungibile: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(api_error(
                StatusCode::BAD_GATEWAY,
                format!("Qdrant create collection fallito (HTTP {status}): {text}"),
            ));
        }
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
///   - `cooldown_seconds_remaining`: se in cooldown attivo
///   - `configured`: se la API key è presente in `settings`
///
/// Differenza con `gateway_providers_handler`: questo NON chiama il gateway
/// (evita loop), e NON richiede auth (è solo lettura aggregata pubblica).
pub async fn providers_status_internal(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
) -> Json<Value> {
    const KNOWN_PROVIDERS: [&str; 5] = ["anthropic", "openai", "google", "deepseek", "mistral"];

    // Ultimo health check per provider.
    #[derive(sqlx::FromRow)]
    struct HealthRow {
        provider: String,
        healthy: bool,
        latency_ms: Option<i32>,
        error_kind: Option<String>,
        checked_at: chrono::DateTime<chrono::Utc>,
    }
    let health_rows: Vec<HealthRow> = sqlx::query_as::<_, HealthRow>(
        r#"SELECT DISTINCT ON (provider)
                  provider, healthy, latency_ms, error_kind, checked_at
           FROM nexus_provider_health_history
           ORDER BY provider, checked_at DESC"#,
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let health_map: std::collections::HashMap<String, HealthRow> = health_rows
        .into_iter()
        .map(|r| (r.provider.clone(), r))
        .collect();

    // API key presenti in settings.
    #[derive(sqlx::FromRow)]
    struct SettingsRow { key: String, value: String }
    let settings_rows: Vec<SettingsRow> = sqlx::query_as::<_, SettingsRow>(
        "SELECT key, value FROM settings WHERE category = 'providers' AND key LIKE '%_api_key'",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let api_key_configured: std::collections::HashMap<String, bool> = settings_rows
        .into_iter()
        .map(|r| (r.key.trim_end_matches("_api_key").to_string(), !r.value.trim().is_empty()))
        .collect();

    // Cooldown snapshot.
    let cooldown_map: std::collections::HashMap<String, (u64, Option<String>)> =
        crate::provider_cooldown::cooldown_snapshot()
            .into_iter()
            .map(|(name, secs, reason)| (name, (secs, reason)))
            .collect();

    let providers: Vec<Value> = KNOWN_PROVIDERS.iter().map(|&name| {
        let mut p = json!({
            "name": name,
            "configured": api_key_configured.get(name).copied().unwrap_or(false),
            "healthy": serde_json::Value::Null,
        });
        if let Some(h) = health_map.get(name) {
            p["healthy"] = json!(h.healthy);
            p["last_check"] = json!(h.checked_at.to_rfc3339());
            if let Some(lat) = h.latency_ms { p["latency_ms"] = json!(lat); }
            if let Some(kind) = &h.error_kind { p["error_kind"] = json!(kind); }
        }
        if let Some((secs, reason)) = cooldown_map.get(name) {
            p["healthy"] = json!(false);
            p["cooldown_seconds_remaining"] = json!(secs);
            if let Some(r) = reason { p["error"] = json!(r); }
        }
        p
    }).collect();

    Json(json!({ "providers": providers }))
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

/// GET /api/admin/providers/budget
/// Ritorna il budget residuo per ogni provider (vista comoda della UI admin).
pub async fn admin_providers_budget_list(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
) -> Json<Value> {
    // Casto NUMERIC -> TEXT in SQL per evitare la dipendenza rust_decimal.
    // sqlx mappa NUMERIC::text -> String senza problemi.
    #[derive(sqlx::FromRow)]
    struct Row {
        provider: String,
        monthly_budget_usd: String,
        spent_current_period_usd: String,
        remaining_usd: String,
        min_threshold_usd: String,
        is_exhausted: bool,
        period_start: chrono::DateTime<chrono::Utc>,
    }
    let rows: Vec<Row> = sqlx::query_as::<_, Row>(
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
    let items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "provider": r.provider,
                "monthly_budget_usd": r.monthly_budget_usd,
                "spent_usd": r.spent_current_period_usd,
                "remaining_usd": r.remaining_usd,
                "min_threshold_usd": r.min_threshold_usd,
                "is_exhausted": r.is_exhausted,
                "period_start": r.period_start.to_rfc3339(),
            })
        })
        .collect();
    Json(json!({ "providers": items }))
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
    Ok(Json(json!({"ok": true, "provider": provider, "monthly_budget_usd": body.monthly_budget_usd})))
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
