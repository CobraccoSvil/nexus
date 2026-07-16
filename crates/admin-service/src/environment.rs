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
        EnvironmentCheck::ok("playwright_libs", "Playwright system libs", "libatk-1.0.so.0 found")
    } else {
        EnvironmentCheck::error("playwright_libs", "Playwright system libs", "libatk-1.0.so.0 missing")
    }
}

#[cfg(windows)]
async fn check_playwright_libs() -> EnvironmentCheck {
    // Su Windows Chromium (Playwright) non richiede librerie .so di sistema:
    // le dipendenze native sono nel bundle del browser. Nessun ldconfig/find:
    // stato sempre OK per non generare falsi allarmi "libreria mancante".
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

fn check_backend_process() -> EnvironmentCheck {
    let pid = std::process::id();
    EnvironmentCheck::ok("backend_process", "Backend mcp-core", format!("pid {pid}"))
}

async fn check_frontend_process() -> EnvironmentCheck {
    // Probe TCP portabile (regola H): un connect riuscito su 127.0.0.1:3000
    // implica che la porta e' in ascolto, indipendentemente dall'OS. Elimina la
    // dipendenza da `ss` (POSIX-only, falso allarme "down" su Windows). Non piu'
    // disponibili pid/program del listener: si riporta solo lo stato in-ascolto.
    const FRONTEND_PORT: u16 = 3000;
    let connect = timeout(
        Duration::from_millis(1000),
        tokio::net::TcpStream::connect(format!("127.0.0.1:{FRONTEND_PORT}")),
    )
    .await;
    match connect {
        Ok(Ok(_)) => EnvironmentCheck::ok(
            "frontend_process",
            "Frontend web-ide",
            format!("Port {FRONTEND_PORT} listening"),
        ),
        Ok(Err(_)) | Err(_) => EnvironmentCheck::error(
            "frontend_process",
            "Frontend web-ide",
            format!("Port {FRONTEND_PORT} not listening"),
        ),
    }
}

async fn check_migrations(db: &sqlx::PgPool) -> EnvironmentCheck {
    // sqlx CLI potrebbe non essere installato. Verifichiamo via DB:
    // la tabella _sqlx_migrations tiene traccia delle migration applicate.
    let result = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM _sqlx_migrations"
    )
    .fetch_one(db)
    .await;

    match result {
        Ok(count) => EnvironmentCheck::ok("migrations", "DB Migrations", format!("{count} applied")),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("does not exist") || msg.contains("relation") {
                EnvironmentCheck::warn("migrations", "DB Migrations", "Migration table not found")
            } else {
                EnvironmentCheck::warn("migrations", "DB Migrations", format!("Check failed: {msg}"))
            }
        }
    }
}

async fn check_ai_providers(db: &sqlx::PgPool) -> EnvironmentCheck {
    // Le chiavi seguono il pattern <provider>_api_key (es. anthropic_api_key, openai_api_key)
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM settings WHERE key LIKE '%_api_key' AND value != '' AND value IS NOT NULL AND value != 'change-me'"
    )
    .fetch_one(db)
    .await
    .unwrap_or(0);

    if count > 0 {
        EnvironmentCheck::ok("ai_providers", "AI Providers", format!("{count} configured"))
    } else {
        EnvironmentCheck::warn("ai_providers", "AI Providers", "0 providers configured")
    }
}

#[cfg(unix)]
async fn check_disk_space() -> EnvironmentCheck {
    // Controlla il disco dove risiede Nexus (non necessariamente il root /)
    let nexus_root = std::env::var("NEXUS_ROOT")
        .unwrap_or_else(|_| "/var/lib/postgresql/wal/nexus".to_string());

    // df output: Filesystem Size Used Avail Use% Mounted on
    let result = Command::new("df").args(["-h", &nexus_root]).output().await;
    let result_root = Command::new("df").args(["-h", "/"]).output().await;

    fn parse_df(out: std::process::Output, label: &str) -> Option<(String, u32)> {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout.lines().nth(1)?;
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 5 { return None; }
        let avail = parts[3];
        let use_pct: u32 = parts[4].trim_end_matches('%').parse().ok()?;
        Some((format!("{label}: {avail} liberi ({use_pct}% usati)"), use_pct))
    }

    let nexus_info = result.ok().and_then(|o| parse_df(o, "nexus"));
    let root_info = result_root.ok().and_then(|o| parse_df(o, "root"));

    // Determina il peggior caso tra i due dischi
    let (detail, max_pct) = match (nexus_info, root_info) {
        (Some((nd, np)), Some((rd, rp))) => {
            if np != rp {
                // Dischi diversi — mostra entrambi
                (format!("{nd} · {rd}"), np.max(rp))
            } else {
                // Stesso disco — mostra uno solo
                (nd, np)
            }
        }
        (Some((nd, np)), None) => (nd, np),
        (None, Some((rd, rp))) => (rd, rp),
        (None, None) => return EnvironmentCheck::warn("disk_space", "Disk space", "df non disponibile"),
    };

    if max_pct >= 95 {
        EnvironmentCheck::error("disk_space", "Disk space", detail)
    } else if max_pct >= 85 {
        EnvironmentCheck::warn("disk_space", "Disk space", detail)
    } else {
        EnvironmentCheck::ok("disk_space", "Disk space", detail)
    }
}

#[cfg(windows)]
async fn check_disk_space() -> EnvironmentCheck {
    // Degrado pulito su Windows: `df` non esiste. admin-service NON dipende da
    // windows-sys (a differenza di mcp-core), quindi non e' disponibile l'API
    // nativa GetDiskFreeSpaceExW senza introdurre una nuova dipendenza. Per non
    // generare un finto errore "df non disponibile", si riporta uno stato
    // esplicito di metrica non misurabile (status ok: non e' un guasto).
    EnvironmentCheck::ok(
        "disk_space",
        "Disk space",
        "metrica non disponibile su Windows",
    )
}

/// Controlla se un servizio HTTP interno risponde all'endpoint /health o /api/health
async fn check_internal_service(id: &str, label: &str, port: u16, health_path: &str) -> EnvironmentCheck {
    let url = format!("http://127.0.0.1:{port}{health_path}");
    let result = timeout(
        Duration::from_secs(3),
        Command::new("curl").args(["-fsS", "--max-time", "2", &url]).output(),
    ).await;

    match result {
        Ok(Ok(out)) if out.status.success() => {
            // Prova a estrarre la versione dal JSON se presente
            let body = String::from_utf8_lossy(&out.stdout);
            let version = if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                v.get("version").and_then(|v| v.as_str()).map(|s| format!(" v{s}")).unwrap_or_default()
            } else { String::new() };
            EnvironmentCheck::ok(id, label, format!(":{port}{version}"))
        }
        _ => EnvironmentCheck::error(id, label, format!(":{port} not responding")),
    }
}

pub async fn get_environment_status(
    State(state): State<AppState>,
) -> ApiResult {
    let (
        db_check,
        playwright_libs_check,
        playwright_browser_check,
        frontend_check,
        migrations_check,
        providers_check,
        disk_check,
        svc_mcp,
        svc_admin,
        svc_doc,
        svc_plugin,
    ) = tokio::join!(
        check_db(&state.db),
        check_playwright_libs(),
        check_playwright_browser(),
        check_frontend_process(),
        check_migrations(&state.db),
        check_ai_providers(&state.db),
        check_disk_space(),
        check_internal_service("svc_mcp_core",    "MCP Core (:4000)",        4000, "/api/health"),
        check_internal_service("svc_admin",        "Admin Service (:4010)",   4010, "/health"),
        check_internal_service("svc_doc",          "Doc Service (:4030)",     4030, "/health"),
        check_internal_service("svc_plugin",       "Plugin Service (:4050)",  4050, "/health"),
    );

    let backend_check = check_backend_process();

    let checks = vec![
        db_check,
        playwright_libs_check,
        playwright_browser_check,
        backend_check,
        frontend_check,
        migrations_check,
        providers_check,
        disk_check,
        // Microservizi interni
        svc_mcp,
        svc_admin,
        svc_doc,
        svc_plugin,
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
            // Le migration vengono applicate automaticamente da mcp-core all'avvio.
            // Qui riavviamo mcp-core per forzarne la riesecuzione.
            let nexus_root = std::env::var("NEXUS_ROOT")
                .unwrap_or_else(|_| "/var/lib/postgresql/wal/nexus".to_string());
            let script = format!("cd {nexus_root} && bash scripts/dev-server-101.sh restart-backend > /tmp/mcp-restart.log 2>&1");
            let result = timeout(
                Duration::from_secs(60),
                Command::new("sh").arg("-c").arg(&script).output(),
            )
            .await;

            match result {
                Ok(Ok(_)) => Ok(Json(json!({ "ok": true, "output": "mcp-core riavviato. Le migration vengono applicate all'avvio." }))),
                Ok(Err(e)) => Ok(Json(json!({ "ok": false, "output": format!("Error: {e}") }))),
                Err(_) => Ok(Json(json!({ "ok": false, "output": "Timeout" }))),
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

        _ => Err(api_error(StatusCode::BAD_REQUEST, format!("Unknown action: {}", body.action))),
    }
}
