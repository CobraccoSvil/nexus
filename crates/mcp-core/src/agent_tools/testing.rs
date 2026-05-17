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
use chrono;

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

fn has_pw_config(dir: &Path) -> bool {
    dir.join("playwright.config.ts").is_file()
        || dir.join("playwright.config.js").is_file()
        || dir.join("playwright.config.mjs").is_file()
}

fn count_spec_files(dir: &Path) -> usize {
    let test_dirs = ["e2e", "tests", "test", "__tests__"];
    let mut count = 0;
    for td in &test_dirs {
        let test_path = dir.join(td);
        if test_path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&test_path) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.ends_with(".spec.ts")
                        || name.ends_with(".spec.js")
                        || name.ends_with(".test.ts")
                        || name.ends_with(".test.js")
                    {
                        count += 1;
                    }
                }
            }
        }
    }
    count
}

/// Sceglie la directory Playwright migliore tra radice e sottodirectory.
/// Quando sia la radice sia una sottodirectory hanno un config, preferisce
/// quella con piu' file spec (la suite reale, non un wrapper semplificato).
/// Scandisce ricorsivamente `test-results/` (e `playwright-report/`) sotto la
/// playwright root, ritorna i path relativi alla project root degli artefatti
/// di interesse: screenshot (png/jpg), video (webm/mp4), trace (zip), HTML report.
fn collect_playwright_artifacts(pw_root: &Path, project_root: &Path) -> Vec<serde_json::Value> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    let dirs = ["test-results", "playwright-report"];

    fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>, depth: u32) {
        if depth > 6 {
            return;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out, depth + 1);
                } else if path.is_file() {
                    out.push(path);
                }
            }
        }
    }

    let mut files: Vec<std::path::PathBuf> = Vec::new();
    for d in &dirs {
        let p = pw_root.join(d);
        if p.is_dir() {
            walk(&p, &mut files, 0);
        }
    }

    for path in files {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let kind = match ext.as_str() {
            "png" | "jpg" | "jpeg" | "webp" => "image",
            "webm" | "mp4" => "video",
            "zip" => "trace",
            "html" if name == "index.html" => "report",
            _ => continue,
        };
        let rel = path
            .strip_prefix(project_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        out.push(serde_json::json!({
            "kind": kind,
            "name": name,
            "path": rel,
            "size": size,
        }));
    }

    // Ordina: prima images (di solito screenshot dei test falliti), poi video, trace, report
    out.sort_by_key(|v| match v.get("kind").and_then(|k| k.as_str()).unwrap_or("") {
        "image" => 0,
        "video" => 1,
        "trace" => 2,
        "report" => 3,
        _ => 4,
    });

    out.truncate(50);
    out
}

/// Ritorna (directory scelta, eventuali config "wrapper" stale da segnalare).
/// Una config wrapper e' un playwright.config.ts alla radice con MOLTI MENO test
/// della sottodirectory scelta (es. residuo di esperimenti precedenti).
fn pick_playwright_root_with_stale(base: &Path) -> (std::path::PathBuf, Vec<std::path::PathBuf>) {
    let subdirs = ["app", "frontend", "client", "web", "packages/web", "src"];

    let mut candidates: Vec<(std::path::PathBuf, usize)> = Vec::new();

    if has_pw_config(base) || base.join("node_modules/@playwright/test").is_dir() {
        candidates.push((base.to_path_buf(), count_spec_files(base)));
    }

    for sub in &subdirs {
        let sub_path = base.join(sub);
        if has_pw_config(&sub_path) || sub_path.join("node_modules/@playwright/test").is_dir() {
            let n = count_spec_files(&sub_path);
            candidates.push((sub_path, n));
        }
    }

    if candidates.is_empty() {
        return (base.to_path_buf(), Vec::new());
    }

    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    let chosen = candidates[0].0.clone();
    let chosen_count = candidates[0].1;

    // Config "wrapper stale": ha un config ma molti meno test della scelta
    // (soglia: almeno 5x in meno, indica residuo di esperimenti/scaffolding).
    let stale: Vec<std::path::PathBuf> = candidates
        .iter()
        .skip(1)
        .filter(|(p, count)| has_pw_config(p) && chosen_count >= 5 * (*count + 1))
        .map(|(p, _)| p.clone())
        .collect();

    (chosen, stale)
}

fn pick_playwright_root(base: &Path) -> std::path::PathBuf {
    pick_playwright_root_with_stale(base).0
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
    let config_path_override = input.get("config_path").and_then(Value::as_str).map(str::to_string);
    let cleanup_stale = input.get("cleanup_stale_configs").and_then(Value::as_bool).unwrap_or(true);

    // ── 2. Controllo presenza Playwright ─────────────────────────────────────
    let (playwright_root, stale_configs): (std::path::PathBuf, Vec<std::path::PathBuf>) =
        if let Some(ref cp) = config_path_override {
            let resolved = ctx.root_path.join(cp);
            let dir = if resolved.is_dir() {
                resolved
            } else if resolved.is_file() {
                resolved.parent().unwrap_or(&ctx.root_path).to_path_buf()
            } else {
                return format!(
                    "[run_playwright_tests] config_path '{}' non trovato. Passa una directory relativa (es. \"app\") o un file config.",
                    cp
                );
            };
            (dir, Vec::new())
        } else {
            pick_playwright_root_with_stale(&ctx.root_path)
        };
    let root = &playwright_root;
    tracing::info!(playwright_root = %root.display(), "run_playwright_tests: directory scelta");

    // Cleanup automatico di config "wrapper stale" alla radice (es. residuo di run precedenti)
    let mut cleanup_notes: Vec<String> = Vec::new();
    if cleanup_stale && ctx.can_write && !stale_configs.is_empty() {
        for stale_dir in &stale_configs {
            for ext in &["ts", "js", "mjs"] {
                let cfg = stale_dir.join(format!("playwright.config.{ext}"));
                if cfg.is_file() {
                    if let Err(e) = std::fs::remove_file(&cfg) {
                        tracing::warn!(path = %cfg.display(), error = %e, "cleanup stale config: errore");
                    } else {
                        cleanup_notes.push(format!("Rimossa config wrapper stale: {}", cfg.display()));
                        tracing::info!(path = %cfg.display(), "cleanup stale config: rimossa");
                    }
                }
            }
            // Rimuovi anche eventuale e2e/ alla radice con solo example.spec.ts
            let e2e_dir = stale_dir.join("e2e");
            if e2e_dir.is_dir() {
                if let Ok(entries) = std::fs::read_dir(&e2e_dir) {
                    let files: Vec<_> = entries.flatten().collect();
                    let only_example = files.len() == 1
                        && files[0].file_name().to_string_lossy().starts_with("example.spec");
                    if only_example {
                        if let Err(e) = std::fs::remove_dir_all(&e2e_dir) {
                            tracing::warn!(path = %e2e_dir.display(), error = %e, "cleanup stale e2e/: errore");
                        } else {
                            cleanup_notes.push(format!("Rimossa directory e2e/ wrapper: {}", e2e_dir.display()));
                        }
                    }
                }
            }
        }
    }

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
    let mut base_url = if let Some(explicit) = explicit_base_url {
        // Override esplicito dall'utente
        Some(explicit)
    } else if let Some(port) = pick_dev_port(&port_rows) {
        Some(format!("http://localhost:{}", port))
    } else {
        None
    };

    // Fallback: se la porta scelta non risponde, prova le altre porte allocate
    if let Some(ref url) = base_url {
        let chosen_port: Option<i32> = url
            .trim_start_matches("http://localhost:")
            .split('/')
            .next()
            .and_then(|s| s.parse().ok());
        if let Some(cp) = chosen_port {
            if !port_reachable(cp).await {
                for (p, _label) in &port_rows {
                    if *p != cp && port_reachable(*p).await {
                        base_url = Some(format!("http://localhost:{}", p));
                        tracing::info!(chosen = cp, fallback = p, "run_playwright_tests: porta scelta non raggiungibile, uso fallback");
                        break;
                    }
                }
            }
        }
    }

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
    tracing::info!(command = %command_str, root = %root.display(), "run_playwright_tests: avvio comando");

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

    // ── Live monitoring: INSERT iniziale + broadcast channel ─────────────────
    let job_id = Uuid::new_v4();
    let _ = sqlx::query(
        "INSERT INTO jobs (id, project_id, kind, status, input, progress, output_log) \
         VALUES ($1, $2, 'playwright_test', 'running', $3, '{}'::jsonb, '')"
    )
    .bind(job_id)
    .bind(ctx.project_id)
    .bind(serde_json::json!({
        "label": "Run in corso...",
        "command": command_str,
        "started_at": chrono::Utc::now().to_rfc3339(),
    }))
    .execute(&*ctx.db)
    .await;
    let _live_tx = crate::playwright_live::register(&ctx.playwright_channels, job_id);
    tracing::info!(job_id = %job_id, "run_playwright_tests: live job registrato");

    // Dispatcher: notifica creazione job → pannello Playwright aggiorna la lista subito
    nexus_events::dispatcher::emit(
        &ctx.project_channels,
        ctx.project_id,
        nexus_events::event::ProjectEvent::JobCreated {
            id: job_id,
            job_kind: "playwright_test".to_string(),
            status: "running".to_string(),
            label: "Run in corso...".to_string(),
            summary: None,
            artifacts: serde_json::Value::Null,
        },
    );

    // ── Raccoglie stdout/stderr IN PARALLELO con child.wait() ──────────────────
    // Stdout: legge riga-per-riga per parsing live + UPDATE incrementale jobs.
    // Stderr: legge a blocchi (per debug aggregato, no parsing live).
    use tokio::io::{AsyncBufReadExt, BufReader};
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();

    let db_for_stdout = ctx.db.clone();
    let channels_for_stdout = ctx.playwright_channels.clone();
    let stdout_task = tokio::spawn(async move {
        let mut full_bytes: Vec<u8> = Vec::new();
        let mut progress = crate::playwright_live::PlaywrightProgress::default();
        let mut acc_log = String::new();
        let mut last_db_flush = std::time::Instant::now();
        const FLUSH_INTERVAL: Duration = Duration::from_millis(500);
        const LOG_MAX_CHARS: usize = 200_000; // tronca per non saturare il DB

        if let Some(out) = stdout_handle {
            let mut reader = BufReader::new(out).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                full_bytes.extend_from_slice(line.as_bytes());
                full_bytes.push(b'\n');

                // Parser live
                let prev_passed = progress.passed;
                let prev_failed = progress.failed;
                crate::playwright_live::parse_line(&line, &mut progress);

                // Accumula log per UPDATE (con cap)
                if acc_log.len() < LOG_MAX_CHARS {
                    acc_log.push_str(&line);
                    acc_log.push('\n');
                }

                // Emette evento Line sempre
                crate::playwright_live::emit(
                    &channels_for_stdout,
                    crate::playwright_live::PlaywrightEvent::Line {
                        job_id,
                        line: line.chars().take(2000).collect(),
                    },
                );

                // Emette evento Progress se i contatori sono cambiati
                if progress.passed != prev_passed || progress.failed != prev_failed {
                    crate::playwright_live::emit(
                        &channels_for_stdout,
                        crate::playwright_live::PlaywrightEvent::Progress {
                            job_id,
                            progress: progress.clone(),
                        },
                    );
                }

                // Flush DB a intervalli (max 500ms tra UPDATE)
                if last_db_flush.elapsed() >= FLUSH_INTERVAL {
                    let _ = sqlx::query(
                        "UPDATE jobs SET output_log = $1, progress = $2 WHERE id = $3"
                    )
                    .bind(&acc_log)
                    .bind(serde_json::to_value(&progress).unwrap_or(serde_json::json!({})))
                    .bind(job_id)
                    .execute(&*db_for_stdout)
                    .await;
                    last_db_flush = std::time::Instant::now();
                }
            }
        }

        // Flush finale (cattura le ultime righe sotto la soglia interval)
        let _ = sqlx::query(
            "UPDATE jobs SET output_log = $1, progress = $2 WHERE id = $3"
        )
        .bind(&acc_log)
        .bind(serde_json::to_value(&progress).unwrap_or(serde_json::json!({})))
        .bind(job_id)
        .execute(&*db_for_stdout)
        .await;

        (full_bytes, progress)
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut err) = stderr_handle {
            let _ = err.read_to_end(&mut buf).await;
        }
        buf
    });

    let timeout_result = tokio::time::timeout(
        Duration::from_secs(timeout),
        child.wait(),
    )
    .await;

    let exit_code = match timeout_result {
        Ok(Ok(status)) => {
            let code = status.code().unwrap_or(-1);
            tracing::info!(exit_code = code, "run_playwright_tests: processo terminato");
            code
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, "run_playwright_tests: errore attesa processo");
            return format!("[run_playwright_tests] Errore attesa processo: {e}");
        }
        Err(_) => {
            tracing::error!(timeout_secs = timeout, "run_playwright_tests: timeout");
            // Tenta kill esplicito per liberare le pipe
            let _ = child.start_kill();
            return format!(
                "[run_playwright_tests] Timeout dopo {}s. I test sono stati interrotti.\n\
                 Considera di aumentare il timeout con timeout_secs o di filtrare i test con il parametro filter.",
                timeout
            );
        }
    };

    // I task lettura terminano quando le pipe si chiudono (alla fine del processo).
    let (stdout_bytes, live_progress) = stdout_task
        .await
        .unwrap_or_else(|_| (Vec::new(), crate::playwright_live::PlaywrightProgress::default()));
    let stderr_bytes = stderr_task.await.unwrap_or_default();
    let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
    let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();

    // ── 8. Parsa statistiche ──────────────────────────────────────────────────
    let stats = parse_playwright_output_stats(&stdout, &stderr);

    // ── 8b. Raccogli artifact (screenshot, video, trace) ─────────────────────
    let artifacts = collect_playwright_artifacts(root, &ctx.root_path);

    // ── 9. Finalizza il record `jobs` (UPDATE, non nuova INSERT) ───────────────
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

        // Progress finale: usa stats parser-completo (piu' affidabile del parser live)
        // ma preserva failed_specs/current_spec accumulati live se disponibili.
        let final_progress = crate::playwright_live::PlaywrightProgress {
            total: live_progress.total.or(Some((stats.passed + stats.failed + stats.skipped) as u32)),
            passed: stats.passed as u32,
            failed: stats.failed as u32,
            skipped: stats.skipped as u32,
            flaky: live_progress.flaky,
            current_spec: None,
            failed_specs: if live_progress.failed_specs.is_empty() {
                stats.failed_tests.iter().take(20).cloned().collect()
            } else {
                live_progress.failed_specs.clone()
            },
        };

        match sqlx::query(
            "UPDATE jobs SET status = $1, input = $2, progress = $3 WHERE id = $4"
        )
        .bind(status)
        .bind(serde_json::json!({
            "label": label,
            "message": msg,
            "artifacts": artifacts,
            "command": command_str,
            "exit_code": exit_code,
        }))
        .bind(serde_json::to_value(&final_progress).unwrap_or(serde_json::json!({})))
        .bind(job_id)
        .execute(&*db)
        .await
        {
            Ok(r) => tracing::info!(rows = r.rows_affected(), project_id = %pid, status = %status, artifacts = artifacts.len(), "playwright_test job aggiornato"),
            Err(e) => tracing::error!(error = %e, project_id = %pid, "playwright_test job UPDATE fallito"),
        }

        // Dispatcher: notifica esito finale → toast + highlight pannello Playwright
        nexus_events::dispatcher::emit(
            &ctx.project_channels,
            ctx.project_id,
            nexus_events::event::ProjectEvent::JobCreated {
                id: job_id,
                job_kind: "playwright_test".to_string(),
                status: status.to_string(),
                label: label.clone(),
                summary: Some(msg.clone()),
                artifacts: serde_json::to_value(&artifacts).unwrap_or(serde_json::Value::Null),
            },
        );

        // Emette evento terminale agli SSE consumer + rimuove channel
        crate::playwright_live::emit(
            &ctx.playwright_channels,
            crate::playwright_live::PlaywrightEvent::Final {
                job_id,
                status: status.to_string(),
                exit_code,
                progress: final_progress,
            },
        );
        // Lascia il channel attivo per qualche secondo: i consumer SSE che si
        // collegano DOPO il termine devono comunque ricevere il Final.
        // Cleanup deferito.
        let channels_cleanup = ctx.playwright_channels.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            crate::playwright_live::unregister(&channels_cleanup, job_id);
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
         Playwright root: {pw_root}\n\
         {cleanup_section}\
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
        pw_root = root.display(),
        cleanup_section = if cleanup_notes.is_empty() {
            String::new()
        } else {
            format!("Cleanup:\n  {}\n", cleanup_notes.join("\n  "))
        },
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

// ── Tool Fase 3: test singolo, lint fix, format file ──────────────────

/// Esegue un singolo test (o un filtro per nome) invece dell'intera suite.
/// Rileva il framework dal progetto: cargo test, pnpm test, pytest.
pub(super) async fn tool_run_specific_test(ctx: &AgentToolContext, input: &Value) -> String {
    let test_name = match input.get("test_name").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s,
        _ => return "[Errore: parametro 'test_name' obbligatorio]".to_string(),
    };
    let working_dir = input
        .get("working_dir")
        .and_then(Value::as_str)
        .unwrap_or("");
    let timeout_secs = input
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(120)
        .min(600);

    let work_path = if working_dir.is_empty() {
        ctx.root_path.clone()
    } else {
        match resolve_relative_path(&ctx.root_path, working_dir) {
            Ok(p) => p,
            Err(e) => return format!("[Errore percorso: {}]", e.1["error"].as_str().unwrap_or("path error")),
        }
    };

    // Rileva il framework di test
    let command = if work_path.join("Cargo.toml").is_file() {
        format!("cargo test {} -- --nocapture 2>&1", test_name)
    } else if work_path.join("package.json").is_file() {
        // Node: pnpm/npm test con filtro
        if work_path.join("vitest.config.ts").is_file()
            || work_path.join("vitest.config.js").is_file()
        {
            format!("npx vitest run -t '{}' 2>&1", test_name)
        } else if work_path.join("jest.config.ts").is_file()
            || work_path.join("jest.config.js").is_file()
        {
            format!("npx jest -t '{}' 2>&1", test_name)
        } else {
            format!("pnpm test -- --grep '{}' 2>&1", test_name)
        }
    } else if work_path.join("pytest.ini").is_file()
        || work_path.join("pyproject.toml").is_file()
        || work_path.join("setup.py").is_file()
    {
        format!("python -m pytest -k '{}' -v 2>&1", test_name)
    } else if work_path.join("mix.exs").is_file() {
        format!("mix test --only {} 2>&1", test_name)
    } else if work_path.join("go.mod").is_file() {
        format!("go test -run '{}' -v ./... 2>&1", test_name)
    } else {
        return format!(
            "[Errore: framework di test non rilevato in '{}'. \
             File cercati: Cargo.toml, package.json, pytest.ini, pyproject.toml, mix.exs, go.mod]",
            work_path.display()
        );
    };

    run_test_command(ctx, &command, &work_path, timeout_secs).await
}

/// Esegue il linter con fix automatico (clippy --fix, eslint --fix, ruff --fix).
pub(super) async fn tool_run_lint_fix(ctx: &AgentToolContext, input: &Value) -> String {
    if !ctx.can_write {
        return "[Errore: permesso di scrittura non concesso]".to_string();
    }
    let check_only = input
        .get("check_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let working_dir = input
        .get("working_dir")
        .and_then(Value::as_str)
        .unwrap_or("");
    let timeout_secs = input
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(120)
        .min(300);

    let work_path = if working_dir.is_empty() {
        ctx.root_path.clone()
    } else {
        match resolve_relative_path(&ctx.root_path, working_dir) {
            Ok(p) => p,
            Err(e) => return format!("[Errore percorso: {}]", e.1["error"].as_str().unwrap_or("path error")),
        }
    };

    let command = if work_path.join("Cargo.toml").is_file() {
        if check_only {
            "cargo clippy --all-targets -- -D warnings 2>&1".to_string()
        } else {
            "cargo clippy --fix --allow-dirty --allow-staged --all-targets 2>&1".to_string()
        }
    } else if work_path.join("package.json").is_file() {
        if check_only {
            "npx eslint . 2>&1".to_string()
        } else {
            "npx eslint . --fix 2>&1".to_string()
        }
    } else if work_path.join("pyproject.toml").is_file()
        || work_path.join("setup.py").is_file()
        || work_path.join("ruff.toml").is_file()
    {
        if check_only {
            "ruff check . 2>&1".to_string()
        } else {
            "ruff check . --fix 2>&1".to_string()
        }
    } else {
        return format!(
            "[Errore: linter non rilevato in '{}'. \
             Supportati: cargo clippy (Rust), eslint (Node), ruff (Python)]",
            work_path.display()
        );
    };

    run_test_command(ctx, &command, &work_path, timeout_secs).await
}

/// Formatta un singolo file (rustfmt, prettier, black) in base all'estensione.
pub(super) async fn tool_format_file(ctx: &AgentToolContext, input: &Value) -> String {
    if !ctx.can_write {
        return "[Errore: permesso di scrittura non concesso]".to_string();
    }
    let path_str = match input.get("path").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s,
        _ => return "[Errore: parametro 'path' obbligatorio]".to_string(),
    };
    let check_only = input
        .get("check_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let target = match resolve_relative_path(&ctx.root_path, path_str) {
        Ok(p) => p,
        Err(e) => return format!("[Errore percorso: {}]", e.1["error"].as_str().unwrap_or("path error")),
    };
    if !target.is_file() {
        return format!("[Errore: '{}' non e' un file]", path_str);
    }

    let ext = target
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let command = match ext.as_str() {
        "rs" => {
            if check_only {
                format!("rustfmt --check '{}' 2>&1", target.display())
            } else {
                format!("rustfmt '{}' 2>&1", target.display())
            }
        }
        "ts" | "tsx" | "js" | "jsx" | "json" | "css" | "scss" | "html" | "vue" | "svelte"
        | "yaml" | "yml" | "md" => {
            if check_only {
                format!("npx prettier --check '{}' 2>&1", target.display())
            } else {
                format!("npx prettier --write '{}' 2>&1", target.display())
            }
        }
        "py" => {
            if check_only {
                format!("black --check '{}' 2>&1", target.display())
            } else {
                format!("black '{}' 2>&1", target.display())
            }
        }
        "go" => format!("gofmt -w '{}' 2>&1", target.display()),
        _ => {
            return format!(
                "[Errore: formatter non disponibile per estensione '.{}'. \
                 Supportati: .rs (rustfmt), .ts/.js/.json/.css/.md (prettier), .py (black), .go (gofmt)]",
                ext
            );
        }
    };

    run_test_command(ctx, &command, &ctx.root_path, 30).await
}

/// Helper comune: esegue un comando con timeout e cattura output.
async fn run_test_command(
    ctx: &AgentToolContext,
    command: &str,
    work_dir: &Path,
    timeout_secs: u64,
) -> String {
    use tokio::io::AsyncReadExt;

    let mut child = match tokio::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(work_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env_clear()
        .envs(crate::sandbox::safe_env_for_direct_spawn())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return format!("[Errore avvio: {}]", e),
    };

    // Lettura stdout/stderr in parallelo con child.wait() per evitare deadlock
    // del buffer pipe (~64 KB Linux).
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut out) = stdout_handle {
            let _ = out.read_to_end(&mut buf).await;
        }
        buf
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut err) = stderr_handle {
            let _ = err.read_to_end(&mut buf).await;
        }
        buf
    });

    let timeout_result = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        child.wait(),
    )
    .await;

    let exit_code = match timeout_result {
        Ok(Ok(status)) => status.code().unwrap_or(-1),
        Ok(Err(e)) => return format!("[Errore attesa processo: {}]", e),
        Err(_) => {
            let _ = child.start_kill();
            return format!("[Timeout dopo {}s. Comando: {}]", timeout_secs, command);
        }
    };

    let stdout_bytes = stdout_task.await.unwrap_or_default();
    let stderr_bytes = stderr_task.await.unwrap_or_default();
    let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
    let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();

    // Tronca output se troppo lungo
    let max_out = 6000;
    let stdout_tail = if stdout.len() > max_out {
        format!("...(troncato)\n{}", &stdout[stdout.len() - max_out..])
    } else {
        stdout
    };
    let stderr_tail = if stderr.len() > max_out {
        format!("...(troncato)\n{}", &stderr[stderr.len() - max_out..])
    } else {
        stderr
    };

    let mut result = format!("Exit code: {}\n", exit_code);
    if !stdout_tail.is_empty() {
        result.push_str(&format!("\nOutput:\n{}", stdout_tail));
    }
    if !stderr_tail.is_empty() {
        result.push_str(&format!("\nErrori:\n{}", stderr_tail));
    }
    result
}
