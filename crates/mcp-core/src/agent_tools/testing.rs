//! Tool agent specializzati per il testing.
//!
//! `run_playwright_tests` — esegue la suite Playwright rispettando le porte
//! allocate da Nexus per il progetto corrente.
//!
//! Flusso:
//! 1. Legge le porte da `nexus_port_allocations` per il progetto.
//! 2. Determina la `BASE_URL` del dev server (porta con label "dev"|"app"|"http"
//!    oppure la porta più bassa allocata).
//! 3. Se `base_url` è passato esplicitamente → usa quello (override).
//! 4. Se non c'è nessuna porta allocata → usa la baseURL in playwright.config.ts
//!    (o il default 3000).
//! 5. Inietta `BASE_URL` come variabile d'ambiente e lancia `npx playwright test`.
//! 6. Salva il risultato in `jobs` (kind = "playwright_test") per il pannello Playwright.

use super::*;
use std::time::Duration;
use tokio::io::AsyncReadExt;

const PLAYWRIGHT_DEFAULT_TIMEOUT: u64 = 600;
const PLAYWRIGHT_MAX_TIMEOUT: u64 = 900;

/// Porta preferita tra quelle allocate al progetto.
/// Priorità: label "dev" > label "app" > label "http" > porta numericamente minore.
fn pick_dev_port(allocations: &[(i32, String)]) -> Option<i32> {
    if allocations.is_empty() {
        return None;
    }
    // Priorità per label
    for preferred in &["dev", "app", "http", "web", "frontend", "serve", "server"] {
        if let Some((port, _)) = allocations
            .iter()
            .find(|(_, label)| label.to_lowercase().contains(preferred))
        {
            return Some(*port);
        }
    }
    // Fallback: porta numericamente minore (prima nel range del progetto)
    allocations.iter().map(|(p, _)| *p).min()
}

/// Porta del backend tra quelle allocate al progetto.
/// Priorità: label che inizia con "backend" > label che inizia con "api" > label "dotnet" > nessuna.
/// Non restituisce mai la stessa porta del dev server frontend.
fn pick_backend_port(allocations: &[(i32, String)], dev_port: Option<i32>) -> Option<i32> {
    for priority_prefix in &["backend", "api-", "api_", "dotnet", "server-api"] {
        if let Some((port, _)) = allocations.iter().find(|(p, label)| {
            let l = label.to_lowercase();
            l.starts_with(priority_prefix) && Some(*p) != dev_port
        }) {
            return Some(*port);
        }
    }
    None
}

/// Verifica (non-blocking) se una porta TCP è aperta sull'host locale.
async fn port_reachable(port: i32) -> bool {
    use tokio::net::TcpStream;
    use std::net::SocketAddr;
    let addr: SocketAddr = format!("127.0.0.1:{}", port).parse().unwrap_or_else(|_| "127.0.0.1:0".parse().unwrap());
    tokio::time::timeout(Duration::from_millis(500), TcpStream::connect(addr))
        .await
        .is_ok_and(|r| r.is_ok())
}

pub(super) async fn tool_run_playwright_tests(ctx: &AgentToolContext, input: &Value) -> String {
    // ── 1. Parametri ─────────────────────────────────────────────────────────
    let filter = input.get("filter").and_then(Value::as_str).map(str::to_string);
    let project_arg = input.get("project").and_then(Value::as_str).map(str::to_string);
    let workers = input.get("workers").and_then(Value::as_u64).unwrap_or(1);
    let reporter = input.get("reporter").and_then(Value::as_str).unwrap_or("list").to_string();
    let explicit_base_url = input.get("base_url").and_then(Value::as_str).map(str::to_string);
    let timeout = input
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(PLAYWRIGHT_DEFAULT_TIMEOUT)
        .min(PLAYWRIGHT_MAX_TIMEOUT);
    // Timeout per il singolo test Playwright (ms). Default 10s: abbastanza per test
    // rapidi (connection refused < 1s) ma non 30s (che causa 42×30=21min su backend down).
    // L'agente può aumentarlo se i test richiedono operazioni lente (upload, rendering).
    let test_timeout_ms = input
        .get("test_timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(10_000)
        .min(60_000);
    let auto_start = input.get("auto_start_server").and_then(Value::as_bool).unwrap_or(false);

    // ── 2. Controllo presenza Playwright ─────────────────────────────────────
    // Cerca playwright.config.ts nella root del progetto, poi nelle sottodirectory
    // comuni (es. app/, frontend/, client/) per supportare monorepo e progetti
    // con struttura a directory (es. redemptor dove il config è in app/).
    let playwright_root: std::path::PathBuf = {
        let base = &ctx.root_path;
        let config_at_base = base.join("playwright.config.ts").is_file()
            || base.join("playwright.config.js").is_file()
            || base.join("playwright.config.mjs").is_file();
        let nm_at_base = base.join("node_modules").join("@playwright").join("test").is_dir();

        if config_at_base || nm_at_base {
            base.clone()
        } else {
            // Cerca in sottodirectory comuni (monorepo / struttura multi-package)
            let candidates = ["app", "frontend", "client", "web", "packages/web", "src"];
            let mut found: Option<std::path::PathBuf> = None;
            for sub in &candidates {
                let sub_path = base.join(sub);
                if sub_path.join("playwright.config.ts").is_file()
                    || sub_path.join("playwright.config.js").is_file()
                    || sub_path.join("node_modules").join("@playwright").join("test").is_dir()
                {
                    found = Some(sub_path);
                    break;
                }
            }
            found.unwrap_or_else(|| base.clone())
        }
    };
    let root = &playwright_root;

    let has_config = root.join("playwright.config.ts").is_file()
        || root.join("playwright.config.js").is_file()
        || root.join("playwright.config.mjs").is_file();
    let has_playwright_nm = root.join("node_modules").join("@playwright").join("test").is_dir();

    if !has_config && !has_playwright_nm {
        return format!(
            "[run_playwright_tests] Playwright non trovato nel progetto (cercato in {} e sottodirectory).\n\
             Installa con: run_command({{\"command\": \"pnpm add -D @playwright/test\", \"working_dir\": \"app\"}}).\n\
             Poi inizializza: run_command({{\"command\": \"npx playwright install --with-deps chromium\", \"working_dir\": \"app\"}}).",
            ctx.root_path.display()
        );
    }

    // ── 3. Leggi porte allocate al progetto dal DB ────────────────────────────
    let port_rows: Vec<(i32, String)> = sqlx::query_as(
        "SELECT port, label FROM nexus_port_allocations WHERE project_id = $1 ORDER BY port ASC",
    )
    .bind(ctx.project_id)
    .fetch_all(&*ctx.db)
    .await
    .unwrap_or_default();

    // ── 4. Determina BASE_URL e BACKEND_API_URL ──────────────────────────────
    let base_url = if let Some(explicit) = explicit_base_url {
        // Override esplicito dall'utente
        Some(explicit)
    } else if let Some(port) = pick_dev_port(&port_rows) {
        Some(format!("http://localhost:{}", port))
    } else {
        None // Playwright userà la baseURL in playwright.config.ts se presente
    };

    // BACKEND_API_URL: porta del servizio backend per il global-setup.ts
    // (seed utenti e health-check pre-test). Non override se già in env.
    let backend_api_url: Option<String> = {
        let dev_port = base_url.as_ref().and_then(|u| {
            u.trim_start_matches("http://localhost:")
                .split('/')
                .next()
                .and_then(|s| s.parse::<i32>().ok())
        });
        pick_backend_port(&port_rows, dev_port)
            .map(|p| format!("http://127.0.0.1:{}", p))
    };

    // ── 5. Verifica se il server è raggiungibile; suggerisci avvio se no ─────
    let server_status = if let Some(ref url) = base_url {
        // Estrai porta dalla URL
        let port: Option<i32> = url
            .trim_start_matches("http://localhost:")
            .trim_start_matches("https://localhost:")
            .split('/')
            .next()
            .and_then(|s| s.parse().ok());

        if let Some(p) = port {
            if port_reachable(p).await {
                format!("Server raggiungibile su {url}")
            } else if auto_start {
                // Avvia il dev server in background tramite run_service
                let start_cmd = detect_dev_server_command(root);
                if let Some(cmd) = start_cmd {
                    let service_input = serde_json::json!({
                        "command": cmd,
                        "label": "Dev Server (auto-start Playwright)",
                    });
                    let svc_result = super::service::tool_run_service(ctx, &service_input, "service").await;
                    // Attendi che il server sia pronto (max 15s)
                    let mut attempts = 0;
                    while attempts < 15 && !port_reachable(p).await {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        attempts += 1;
                    }
                    if port_reachable(p).await {
                        format!("Dev server avviato automaticamente su {url}. Output: {}", svc_result.chars().take(200).collect::<String>())
                    } else {
                        format!("ATTENZIONE: Dev server avviato ma {url} non risponde ancora dopo 15s. I test potrebbero fallire.")
                    }
                } else {
                    format!("ATTENZIONE: {url} non raggiungibile e il comando di avvio non è stato rilevato. Avvia il server con run_service prima di eseguire i test.")
                }
            } else {
                format!("ATTENZIONE: Il server su {url} non risponde. Assicurati che il dev server sia in esecuzione prima dei test.\nSuggerimento: usa run_service con il comando di avvio del progetto, poi ri-esegui i test.\nAlternativamente, passa auto_start_server: true per avvio automatico.")
            }
        } else {
            format!("BASE_URL impostata a {url}")
        }
    } else {
        "Nessuna porta allocata trovata: Playwright userà la baseURL da playwright.config.ts".to_string()
    };

    // ── 6. Costruisci il comando Playwright ───────────────────────────────────
    let mut cmd_parts = vec![
        "npx".to_string(),
        "playwright".to_string(),
        "test".to_string(),
        "--timeout".to_string(),
        test_timeout_ms.to_string(),
        "--workers".to_string(),
        workers.to_string(),
        "--reporter".to_string(),
        reporter.clone(),
    ];

    if let Some(ref p) = project_arg {
        cmd_parts.push("--project".to_string());
        cmd_parts.push(p.clone());
    }

    if let Some(ref f) = filter {
        cmd_parts.push(f.clone());
    }

    let command_str = cmd_parts.join(" ");

    // ── 7. Esegui con env BASE_URL ────────────────────────────────────────────
    let mut child_builder = tokio::process::Command::new("/bin/sh");
    child_builder
        .arg("-c")
        .arg(&command_str)
        .current_dir(root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env("CI", "1")          // headless browser guarantee
        .env("FORCE_COLOR", "0"); // no ANSI codes nell'output

    // Inietta BASE_URL solo se l'abbiamo determinata
    if let Some(ref url) = base_url {
        child_builder.env("BASE_URL", url);
        child_builder.env("PLAYWRIGHT_BASE_URL", url); // compatibilità con alcuni config
    }

    // Inietta BACKEND_API_URL per global-setup.ts (seed utenti, health-check).
    // Override solo se non già presente nell'ambiente del processo.
    if let Some(ref burl) = backend_api_url {
        if std::env::var("BACKEND_API_URL").is_err() {
            child_builder.env("BACKEND_API_URL", burl);
        }
    }

    // Inietta LD_LIBRARY_PATH per dipendenze di sistema di Chromium (libnspr4, libnss3, ecc.)
    // che potrebbero non essere installate globalmente nel sistema.
    // Il path base viene da PLAYWRIGHT_LIBS_PATH oppure da ~/.local/playwright-libs (default).
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let playwright_libs = std::env::var("PLAYWRIGHT_LIBS_PATH")
            .unwrap_or_else(|_| format!("{}/.local/playwright-libs", home));
        if std::path::Path::new(&playwright_libs).exists() {
            let new_ld = match std::env::var("LD_LIBRARY_PATH") {
                Ok(existing) if !existing.is_empty() => {
                    format!("{}:{}", playwright_libs, existing)
                }
                _ => playwright_libs,
            };
            child_builder.env("LD_LIBRARY_PATH", new_ld);
        }
    }

    let mut child = match child_builder.spawn() {
        Ok(c) => c,
        Err(e) => return format!("[run_playwright_tests] Errore avvio processo: {e}"),
    };

    // Raccoglie stdout/stderr in parallelo per evitare deadlock da buffer pieno
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();

    let timeout_result = tokio::time::timeout(
        Duration::from_secs(timeout),
        child.wait(),
    )
    .await;

    let exit_code = match timeout_result {
        Ok(Ok(status)) => status.code().unwrap_or(-1),
        Ok(Err(e)) => return format!("[run_playwright_tests] Errore attesa processo: {e}"),
        Err(_) => {
            return format!(
                "[run_playwright_tests] Timeout dopo {}s. I test sono stati interrotti.\n\
                 Considera di aumentare il timeout con timeout_secs o di filtrare i test con il parametro filter.",
                timeout
            );
        }
    };

    let stdout = if let Some(mut out) = stdout_handle {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).to_string()
    } else {
        String::new()
    };

    let stderr = if let Some(mut err) = stderr_handle {
        let mut buf = Vec::new();
        let _ = err.read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).to_string()
    } else {
        String::new()
    };

    // ── 8. Parsa statistiche ──────────────────────────────────────────────────
    let stats = parse_playwright_output_stats(&stdout, &stderr);

    // ── 9. Salva in jobs per il pannello Playwright ───────────────────────────
    {
        let db = ctx.db.clone();
        let pid = ctx.project_id;
        let status = if exit_code == 0 { "passed" } else { "failed" };
        let label = if exit_code == 0 {
            format!("{} test passati", stats.passed)
        } else {
            format!("{} passati, {} falliti", stats.passed, stats.failed)
        };
        let msg = format!(
            "{}/{} test passati{}",
            stats.passed,
            stats.passed + stats.failed + stats.skipped,
            if stats.failed > 0 {
                format!(". Falliti: {}", stats.failed_tests.iter().take(5).cloned().collect::<Vec<_>>().join(", "))
            } else {
                String::new()
            }
        );
        tokio::spawn(async move {
            let _ = sqlx::query(
                "INSERT INTO jobs (project_id, kind, status, input) VALUES ($1, 'playwright_test', $2, $3)"
            )
            .bind(pid)
            .bind(status)
            .bind(serde_json::json!({ "label": label, "message": msg }))
            .execute(&*db)
            .await;
        });
    }

    // ── 10. Output finale ─────────────────────────────────────────────────────
    let stdout_tail: String = stdout
        .lines()
        .rev()
        .take(60)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");

    let stderr_excerpt: String = stderr
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(20)
        .collect::<Vec<_>>()
        .join("\n");

    let status_label = if exit_code == 0 { "TUTTI I TEST PASSATI" } else { "TEST FALLITI" };
    let port_info = port_rows
        .iter()
        .map(|(p, l)| if l.is_empty() { format!(":{}", p) } else { format!(":{} ({})", p, l) })
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "=== PLAYWRIGHT TEST ===\n\
         Stato: {status_label} (exit code: {exit_code})\n\
         Porte progetto: {port_info}\n\
         BASE_URL: {base_url_display}\n\
         BACKEND_API_URL: {backend_api_url_display}\n\
         Server: {server_status}\n\
         Comando: {command_str}\n\n\
         Risultati:\n\
           Passati:  {passed}\n\
           Falliti:  {failed}\n\
           Saltati:  {skipped}\n\
           Totale:   {total}\n\
         {failed_list}\n\
         --- Output ---\n\
         {stdout_tail}\n\
         {stderr_section}",
        status_label = status_label,
        exit_code = exit_code,
        port_info = if port_info.is_empty() { "nessuna porta allocata".to_string() } else { port_info },
        base_url_display = base_url.as_deref().unwrap_or("(da playwright.config.ts)"),
        backend_api_url_display = backend_api_url.as_deref().unwrap_or("(non trovata — verifica label 'backend-*' in nexus_port_allocations)"),
        server_status = server_status,
        command_str = command_str,
        passed = stats.passed,
        failed = stats.failed,
        skipped = stats.skipped,
        total = stats.passed + stats.failed + stats.skipped,
        failed_list = if stats.failed_tests.is_empty() {
            String::new()
        } else {
            format!("Test falliti:\n{}", stats.failed_tests.iter().map(|t| format!("  - {t}")).collect::<Vec<_>>().join("\n"))
        },
        stdout_tail = stdout_tail,
        stderr_section = if stderr_excerpt.is_empty() {
            String::new()
        } else {
            format!("--- Errori/Warning ---\n{stderr_excerpt}")
        },
    )
}

/// Rileva il comando di avvio del dev server dal package.json / stack del progetto.
fn detect_dev_server_command(root: &std::path::Path) -> Option<String> {
    // Node/Next.js/Vite
    if root.join("package.json").is_file() {
        if let Ok(content) = std::fs::read_to_string(root.join("package.json")) {
            if let Ok(json) = serde_json::from_str::<Value>(&content) {
                let scripts = json.get("scripts")?;
                // Ordine di preferenza: dev > start > serve
                for script in &["dev", "start", "serve", "preview"] {
                    if scripts.get(script).and_then(Value::as_str).is_some() {
                        return Some(format!("pnpm run {} 2>&1", script));
                    }
                }
            }
        }
    }
    // Python/FastAPI/Django
    if root.join("manage.py").is_file() {
        return Some("python manage.py runserver 0.0.0.0:8000".to_string());
    }
    if root.join("pyproject.toml").is_file() || root.join("requirements.txt").is_file() {
        return Some("uvicorn main:app --host 0.0.0.0".to_string());
    }
    None
}

/// Parsa le statistiche dall'output testuale di `npx playwright test`.
fn parse_playwright_output_stats(stdout: &str, stderr: &str) -> PlaywrightStats {
    let combined = format!("{stdout}\n{stderr}");
    let mut stats = PlaywrightStats::default();

    for line in combined.lines() {
        let t = line.trim();

        // "  3 passed (5s)"  oppure "  2 passed, 1 failed (12s)"
        for kw in &["passed", "failed", "skipped", "flaky"] {
            if let Some(n) = extract_stat(t, kw) {
                match *kw {
                    "passed" => stats.passed += n,
                    "failed" => stats.failed += n,
                    "skipped" => stats.skipped += n,
                    "flaky" => stats.flaky += n,
                    _ => {}
                }
            }
        }

        // Righe di test fallito: "    ✘ 1 [chromium] › file.spec.ts:5:3 › test name"
        if (t.contains('✘') || t.contains("FAILED")) && t.contains('›') {
            stats.failed_tests.push(t.chars().take(200).collect());
        }
    }

    stats
}

fn extract_stat(line: &str, keyword: &str) -> Option<usize> {
    let pos = line.find(keyword)?;
    line[..pos]
        .split_whitespace()
        .last()?
        .trim_matches(',')
        .parse()
        .ok()
}

#[derive(Default)]
struct PlaywrightStats {
    passed: usize,
    failed: usize,
    skipped: usize,
    flaky: usize,
    failed_tests: Vec<String>,
}
