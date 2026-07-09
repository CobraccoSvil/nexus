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

/// Pre-flight check: lancia `ldd` sul binary chromium-headless-shell di
/// Playwright e raccoglie la lista delle librerie sistema marcate "not found".
/// Ritorna `None` se tutto ok, `Some(libs)` con i nomi delle librerie mancanti.
///
/// Logica:
/// - Risolve `$HOME/.cache/ms-playwright/chromium_headless_shell-*/chrome-headless-shell-linux64/chrome-headless-shell`
///   (glob sulla versione installata: `1223`, futuro `1240`, ecc.).
/// - Se il binary non esiste -> ritorna None (Playwright non installato; il
///   check precedente has_playwright_nm gestisce gia' il caso).
/// - Esegue `ldd <binary>`, parsa le righe "X => not found", restituisce la
///   lista distinct (es. ["libnspr4.so", "libnss3.so", ...]).
/// - Best-effort: errori ldd / glob falliti ritornano None (no falsi positivi).
///
/// Linux-only: le dipendenze condivise (`.so`) e lo strumento `ldd` esistono solo
/// su Linux. Su Windows i browser Playwright non hanno dipendenze `.so` da
/// verificare: il ramo dedicato ritorna sempre None (nessuna libreria mancante),
/// cosi' il preflight non blocca mai l'esecuzione dei test.
#[cfg(unix)]
async fn preflight_check_chromium_libs() -> Option<Vec<String>> {
    let binary = locate_chromium_headless_binary().await?;

    let out = match tokio::process::Command::new("ldd")
        .arg(&binary)
        .output()
        .await
    {
        Ok(o) => o,
        Err(_) => return None, // ldd assente sul sistema, skip check
    };
    if !out.status.success() {
        return None;
    }
    let missing = parse_ldd_missing_libs(&String::from_utf8_lossy(&out.stdout));
    if missing.is_empty() {
        None
    } else {
        Some(missing)
    }
}

/// Risolve il path del binario `chrome-headless-shell` nella cache Playwright
/// (glob sulla versione installata). Ritorna None se cache o binario assenti.
#[cfg(unix)]
async fn locate_chromium_headless_binary() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let cache_root = format!("{home}/.cache/ms-playwright");
    let mut entries = tokio::fs::read_dir(&cache_root).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("chromium_headless_shell-") {
            let candidate = entry
                .path()
                .join("chrome-headless-shell-linux64")
                .join("chrome-headless-shell");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Estrae dalle righe di `ldd` le librerie marcate "=> not found", distinte
/// nell'ordine di apparizione (es. ["libnspr4.so", "libnss3.so", ...]).
#[cfg(unix)]
fn parse_ldd_missing_libs(stdout: &str) -> Vec<String> {
    let mut missing: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let l = line.trim();
        if l.contains("=> not found") {
            // Format tipo "    libnspr4.so => not found"
            let lib = l.split("=>").next().unwrap_or("").trim().to_string();
            if !lib.is_empty() && !missing.contains(&lib) {
                missing.push(lib);
            }
        }
    }
    missing
}

/// Ramo Windows: nessuna dipendenza `.so` da verificare per Chromium. Ritorna
/// sempre None (nessuna libreria mancante) per non bloccare i test Playwright.
#[cfg(windows)]
async fn preflight_check_chromium_libs() -> Option<Vec<String>> {
    None
}

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
    use std::net::SocketAddr;
    use tokio::net::TcpStream;
    let addr: SocketAddr = format!("127.0.0.1:{}", port)
        .parse()
        .unwrap_or_else(|_| "127.0.0.1:0".parse().unwrap());
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
    let dirs = ["test-results", "playwright-report"];

    let mut files: Vec<std::path::PathBuf> = Vec::new();
    for d in &dirs {
        let p = pw_root.join(d);
        if p.is_dir() {
            walk_artifact_dir(&p, &mut files, 0);
        }
    }

    let mut out: Vec<serde_json::Value> = files
        .iter()
        .filter_map(|path| classify_artifact(path, project_root))
        .collect();

    // Ordina: prima images (di solito screenshot dei test falliti), poi video, trace, report
    out.sort_by_key(
        |v| match v.get("kind").and_then(|k| k.as_str()).unwrap_or("") {
            "image" => 0,
            "video" => 1,
            "trace" => 2,
            "report" => 3,
            _ => 4,
        },
    );

    out.truncate(50);
    out
}

/// Raccoglie ricorsivamente (fino a depth 6) i file regolari sotto `dir`.
fn walk_artifact_dir(dir: &Path, out: &mut Vec<std::path::PathBuf>, depth: u32) {
    if depth > 6 {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk_artifact_dir(&path, out, depth + 1);
            } else if path.is_file() {
                out.push(path);
            }
        }
    }
}

/// Classifica un singolo file come artefatto Playwright (image/video/trace/report)
/// restituendo il descrittore JSON, oppure None se l'estensione non e' rilevante.
fn classify_artifact(path: &Path, project_root: &Path) -> Option<serde_json::Value> {
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
        _ => return None,
    };
    let rel = path
        .strip_prefix(project_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Some(serde_json::json!({
        "kind": kind,
        "name": name,
        "path": rel,
        "size": size,
    }))
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

    candidates.sort_by_key(|c| std::cmp::Reverse(c.1));
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

/// Parametri normalizzati del tool `run_playwright_tests`, estratti da `input`.
struct PlaywrightRunParams {
    filter: Option<String>,
    project_arg: Option<String>,
    workers: u64,
    reporter: String,
    explicit_base_url: Option<String>,
    timeout: u64,
    test_timeout_ms: u64,
    auto_start: bool,
    config_path_override: Option<String>,
    cleanup_stale: bool,
}

/// Estrae e normalizza i parametri dal JSON di input, applicando default e
/// clamp dei timeout (punto unico del parsing input, regola L).
fn parse_playwright_params(input: &Value) -> PlaywrightRunParams {
    PlaywrightRunParams {
        filter: input
            .get("filter")
            .and_then(Value::as_str)
            .map(str::to_string),
        project_arg: input
            .get("project")
            .and_then(Value::as_str)
            .map(str::to_string),
        workers: input.get("workers").and_then(Value::as_u64).unwrap_or(1),
        reporter: input
            .get("reporter")
            .and_then(Value::as_str)
            .unwrap_or("list")
            .to_string(),
        explicit_base_url: input
            .get("base_url")
            .and_then(Value::as_str)
            .map(str::to_string),
        timeout: input
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(PLAYWRIGHT_DEFAULT_TIMEOUT)
            .min(PLAYWRIGHT_MAX_TIMEOUT),
        // Timeout per il singolo test Playwright (ms). Default 10s: abbastanza per test
        // rapidi (connection refused < 1s) ma non 30s (che causa 42×30=21min su backend down).
        // L'agente può aumentarlo se i test richiedono operazioni lente (upload, rendering).
        test_timeout_ms: input
            .get("test_timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(10_000)
            .min(60_000),
        auto_start: input
            .get("auto_start_server")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        config_path_override: input
            .get("config_path")
            .and_then(Value::as_str)
            .map(str::to_string),
        cleanup_stale: input
            .get("cleanup_stale_configs")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    }
}

/// Risolve la playwright root e la lista di config wrapper stale. Se
/// `config_path_override` e' passato, lo risolve (bloccando il traversal);
/// altrimenti delega a `pick_playwright_root_with_stale`. In caso di
/// config_path invalido ritorna Err con il messaggio pronto per il return.
fn resolve_playwright_root(
    ctx: &AgentToolContext,
    config_path_override: &Option<String>,
) -> Result<(std::path::PathBuf, Vec<std::path::PathBuf>), String> {
    let Some(cp) = config_path_override else {
        return Ok(pick_playwright_root_with_stale(&ctx.root_path));
    };
    // Punto unico (regola L): de-duplica la root se l'agente l'ha inclusa
    // in config_path e blocca il traversal (resolve_relative_path).
    let not_found = || {
        format!(
            "[run_playwright_tests] config_path '{}' non trovato. Passa una directory relativa (es. \"app\") o un file config.",
            cp
        )
    };
    let resolved = resolve_relative_path(&ctx.root_path, cp).map_err(|_| not_found())?;
    let dir = if resolved.is_dir() {
        resolved
    } else if resolved.is_file() {
        resolved.parent().unwrap_or(&ctx.root_path).to_path_buf()
    } else {
        return Err(not_found());
    };
    Ok((dir, Vec::new()))
}

/// Rimuove le config "wrapper stale" (playwright.config.* + e2e/ con solo
/// example.spec) alla radice dei candidati scartati, accumulando note testuali.
fn cleanup_stale_configs(stale_configs: &[std::path::PathBuf]) -> Vec<String> {
    let mut cleanup_notes: Vec<String> = Vec::new();
    for stale_dir in stale_configs {
        for ext in &["ts", "js", "mjs"] {
            let cfg = stale_dir.join(format!("playwright.config.{ext}"));
            if !cfg.is_file() {
                continue;
            }
            if let Err(e) = std::fs::remove_file(&cfg) {
                tracing::warn!(path = %cfg.display(), error = %e, "cleanup stale config: errore");
            } else {
                cleanup_notes.push(format!("Rimossa config wrapper stale: {}", cfg.display()));
                tracing::info!(path = %cfg.display(), "cleanup stale config: rimossa");
            }
        }
        cleanup_stale_e2e_dir(stale_dir, &mut cleanup_notes);
    }
    cleanup_notes
}

/// Rimuove la directory `e2e/` di un wrapper stale se contiene solo un file
/// `example.spec*` (residuo di scaffolding). Aggiorna `notes`.
fn cleanup_stale_e2e_dir(stale_dir: &Path, notes: &mut Vec<String>) {
    let e2e_dir = stale_dir.join("e2e");
    if !e2e_dir.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(&e2e_dir) else {
        return;
    };
    let files: Vec<_> = entries.flatten().collect();
    let only_example = files.len() == 1
        && files[0]
            .file_name()
            .to_string_lossy()
            .starts_with("example.spec");
    if !only_example {
        return;
    }
    if let Err(e) = std::fs::remove_dir_all(&e2e_dir) {
        tracing::warn!(path = %e2e_dir.display(), error = %e, "cleanup stale e2e/: errore");
    } else {
        notes.push(format!(
            "Rimossa directory e2e/ wrapper: {}",
            e2e_dir.display()
        ));
    }
}

/// Legge il toggle DB `agent.testing.preflight_check_enabled` (default true).
async fn preflight_enabled(ctx: &AgentToolContext) -> bool {
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'agent.testing.preflight_check_enabled'",
    )
    .fetch_optional(&*ctx.db)
    .await
    .ok()
    .flatten()
    .map(|v| {
        !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "false" | "0" | "off" | "no" | ""
        )
    })
    .unwrap_or(true)
}

/// Tenta l'autofix delle librerie mancanti via Sudo Manager (ADR 0017). Ritorna
/// true se dopo l'autofix le librerie risultano presenti.
async fn try_preflight_autofix(ctx: &AgentToolContext, missing_count: usize) -> bool {
    match crate::sudo_manager::is_executable(&ctx.db, "playwright-install-deps").await {
        Ok(true) => {
            tracing::info!(
                "preflight: tentativo autofix via sudo_manager::execute(playwright-install-deps), {} libs mancanti",
                missing_count
            );
            match crate::sudo_manager::execute(&ctx.db, "playwright-install-deps").await {
                Ok(o) if o.success => {
                    tracing::info!(
                        "preflight: autofix completato (exit=0, duration_ms={}), rivalidazione ldd",
                        o.duration_ms
                    );
                    // Rivalida: se ora le librerie sono presenti, ok.
                    preflight_check_chromium_libs().await.is_none()
                }
                Ok(o) => {
                    tracing::warn!(
                        "preflight: autofix exit_code={} stderr_excerpt={}",
                        o.exit_code,
                        o.stderr.chars().take(200).collect::<String>()
                    );
                    false
                }
                Err(e) => {
                    tracing::warn!("preflight: sudo_manager::execute fallita: {}", e);
                    false
                }
            }
        }
        _ => false,
    }
}

/// Messaggio di blocco quando le librerie di sistema di Chromium mancano e
/// l'autofix non e' disponibile o non e' riuscito.
fn preflight_blocked_message(missing: &[String], root: &Path) -> String {
    format!(
        "[run_playwright_tests] BLOCKED — chromium-headless-shell non puo' avviarsi: {} librerie di sistema mancanti.\n\n\
        Librerie not found dal binary: {}\n\n\
        FIX (uno qualunque):\n\
          (1) ADR 0017 Sudo Manager: bash deploy/install-sudo-manager.sh\n\
              poi Admin UI -> Sudo Manager -> Esegui 'playwright-install-deps'\n\
              (oppure il preflight riprovera' da solo al prossimo run)\n\
          (2) Manuale: sudo apt-get install -y libnspr4 libnss3 libnssutil3 libasound2t64 libxss1 libgbm1 libgtk-3-0 libpangocairo-1.0-0 libatk1.0-0t64 libatk-bridge2.0-0t64 libcups2t64 libxshmfence1\n\
          (3) Playwright nativo: cd {} && npx playwright install-deps chromium\n\n\
        Nessun processo Playwright e' stato lanciato (zero job zombie).",
        missing.len(), missing.join(", "), root.display()
    )
}

/// Pre-flight check delle librerie di sistema di chromium-headless-shell.
/// Regola H: fail-fast esplicito se mancano libnss3/libnspr4/libasound2/...
/// invece di lasciare playwright lanciare 13×3 launch falliti in 1ms ciascuno
/// e produrre un job zombie running per minuti senza progresso (incident
/// Beauty-Book chat 6: 6:49 min senza UPDATE, browser non avviato mai).
/// Toggle via setting agent.testing.preflight_check_enabled (default true).
/// Ritorna Err(messaggio) se il run va bloccato.
async fn run_preflight_check(ctx: &AgentToolContext, root: &Path) -> Result<(), String> {
    if !preflight_enabled(ctx).await {
        return Ok(());
    }
    let Some(missing) = preflight_check_chromium_libs().await else {
        return Ok(());
    };
    // ADR 0017: se Sudo Manager e' installato + purpose
    // `playwright-install-deps` e' executable, tentiamo l'autofix.
    if try_preflight_autofix(ctx, missing.len()).await {
        tracing::info!("preflight: autofix riuscito, procedo con il run playwright");
        return Ok(());
    }
    Err(preflight_blocked_message(&missing, root))
}

/// Estrae la porta numerica da una URL `http(s)://localhost:PORT/...`.
fn port_from_localhost_url(url: &str) -> Option<i32> {
    url.trim_start_matches("http://localhost:")
        .trim_start_matches("https://localhost:")
        .split('/')
        .next()
        .and_then(|s| s.parse().ok())
}

/// Determina la BASE_URL: override esplicito, oppure la porta dev preferita.
/// Se la porta scelta non risponde, prova le altre porte allocate (fallback).
async fn determine_base_url(
    explicit_base_url: Option<String>,
    port_rows: &[(i32, String)],
) -> Option<String> {
    let mut base_url = match explicit_base_url {
        Some(explicit) => Some(explicit), // Override esplicito dall'utente
        None => pick_dev_port(port_rows).map(|port| format!("http://localhost:{}", port)),
    };

    // Fallback: se la porta scelta non risponde, prova le altre porte allocate
    let Some(url) = base_url.as_ref() else {
        return base_url;
    };
    let Some(cp) = port_from_localhost_url(url) else {
        return base_url;
    };
    if !port_reachable(cp).await {
        for (p, _label) in port_rows {
            if *p != cp && port_reachable(*p).await {
                base_url = Some(format!("http://localhost:{}", p));
                tracing::info!(
                    chosen = cp,
                    fallback = p,
                    "run_playwright_tests: porta scelta non raggiungibile, uso fallback"
                );
                break;
            }
        }
    }
    base_url
}

/// Avvia il dev server in background (run_service) e attende che la porta `p`
/// risponda (max 15s). Ritorna la stringa di stato descrittiva.
async fn auto_start_dev_server(ctx: &AgentToolContext, root: &Path, url: &str, p: i32) -> String {
    let Some(cmd) = detect_dev_server_command(root) else {
        return format!("ATTENZIONE: {url} non raggiungibile e il comando di avvio non è stato rilevato. Avvia il server con run_service prima di eseguire i test.");
    };
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
        format!(
            "Dev server avviato automaticamente su {url}. Output: {}",
            svc_result.chars().take(200).collect::<String>()
        )
    } else {
        format!("ATTENZIONE: Dev server avviato ma {url} non risponde ancora dopo 15s. I test potrebbero fallire.")
    }
}

/// Verifica se il server della BASE_URL è raggiungibile, avviandolo se
/// `auto_start` e' attivo. Ritorna il messaggio di stato per l'output finale.
async fn check_server_status(
    ctx: &AgentToolContext,
    root: &Path,
    base_url: Option<&str>,
    auto_start: bool,
) -> String {
    let Some(url) = base_url else {
        return "Nessuna porta allocata trovata: Playwright userà la baseURL da playwright.config.ts"
            .to_string();
    };
    let Some(p) = port_from_localhost_url(url) else {
        return format!("BASE_URL impostata a {url}");
    };
    if port_reachable(p).await {
        format!("Server raggiungibile su {url}")
    } else if auto_start {
        auto_start_dev_server(ctx, root, url, p).await
    } else {
        format!("ATTENZIONE: Il server su {url} non risponde. Assicurati che il dev server sia in esecuzione prima dei test.\nSuggerimento: usa run_service con il comando di avvio del progetto, poi ri-esegui i test.\nAlternativamente, passa auto_start_server: true per avvio automatico.")
    }
}

/// Costruisce la riga di comando `npx playwright test ...` con timeout, workers,
/// reporter e gli argomenti opzionali project/filter.
fn build_playwright_command(
    test_timeout_ms: u64,
    workers: u64,
    reporter: &str,
    project_arg: &Option<String>,
    filter: &Option<String>,
) -> String {
    let mut cmd_parts = vec![
        "npx".to_string(),
        "playwright".to_string(),
        "test".to_string(),
        "--timeout".to_string(),
        test_timeout_ms.to_string(),
        "--workers".to_string(),
        workers.to_string(),
        "--reporter".to_string(),
        reporter.to_string(),
    ];
    if let Some(p) = project_arg {
        cmd_parts.push("--project".to_string());
        cmd_parts.push(p.clone());
    }
    if let Some(f) = filter {
        cmd_parts.push(f.clone());
    }
    cmd_parts.join(" ")
}

/// Calcola il valore di `LD_LIBRARY_PATH` per Chromium: prepende la dir delle
/// librerie Playwright (da PLAYWRIGHT_LIBS_PATH o ~/.local/playwright-libs) al
/// path esistente. Ritorna None se la dir non esiste (nessuna iniezione).
fn playwright_ld_library_path() -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let playwright_libs = std::env::var("PLAYWRIGHT_LIBS_PATH")
        .unwrap_or_else(|_| format!("{}/.local/playwright-libs", home));
    if !std::path::Path::new(&playwright_libs).exists() {
        return None;
    }
    let ld = match std::env::var("LD_LIBRARY_PATH") {
        Ok(existing) if !existing.is_empty() => format!("{}:{}", playwright_libs, existing),
        _ => playwright_libs,
    };
    Some(ld)
}

/// Prepara e lancia il processo Playwright con l'env isolato (punto unico
/// regola L: env_clear + host env filtrato), iniettando BASE_URL/BACKEND_API_URL
/// e LD_LIBRARY_PATH sopra l'env gia' pulito.
fn spawn_playwright_child(
    command_str: &str,
    root: &Path,
    base_url: Option<&str>,
    backend_api_url: Option<&str>,
) -> std::io::Result<tokio::process::Child> {
    // isolated_command (punto unico, regola L): env_clear + host env filtrato —
    // Playwright non eredita i segreti Nexus; BASE_URL/CI/LD_LIBRARY_PATH sono
    // iniettate esplicitamente sotto, sopra l'env gia' pulito.
    let mut child_builder = crate::sandbox::isolated_command(&crate::sandbox::agent_shell());
    child_builder
        .arg("-c")
        .arg(command_str)
        .current_dir(root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env("CI", "1") // headless browser guarantee
        .env("FORCE_COLOR", "0"); // no ANSI codes nell'output

    // Inietta BASE_URL solo se l'abbiamo determinata
    if let Some(url) = base_url {
        child_builder.env("BASE_URL", url);
        child_builder.env("PLAYWRIGHT_BASE_URL", url); // compatibilità con alcuni config
    }

    // Inietta BACKEND_API_URL per global-setup.ts (seed utenti, health-check).
    // Override solo se non già presente nell'ambiente del processo.
    if let Some(burl) = backend_api_url {
        if std::env::var("BACKEND_API_URL").is_err() {
            child_builder.env("BACKEND_API_URL", burl);
        }
    }

    // Inietta LD_LIBRARY_PATH per dipendenze di sistema di Chromium (libnspr4, libnss3, ecc.)
    // che potrebbero non essere installate globalmente nel sistema.
    if let Some(new_ld) = playwright_ld_library_path() {
        child_builder.env("LD_LIBRARY_PATH", new_ld);
    }

    child_builder.spawn()
}

/// Legge lo stdout del processo Playwright riga-per-riga: parsing live dei
/// contatori, emissione eventi (Line/Progress) e flush incrementale del record
/// `jobs` (max ogni 500ms). Ritorna (byte grezzi accumulati, progress finale).
async fn stream_playwright_stdout(
    stdout_handle: Option<tokio::process::ChildStdout>,
    db: sqlx::PgPool,
    channels: crate::playwright_live::PlaywrightChannels,
    job_id: Uuid,
) -> (Vec<u8>, crate::playwright_live::PlaywrightProgress) {
    use tokio::io::{AsyncBufReadExt, BufReader};
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

            emit_stdout_line_events(
                &channels,
                job_id,
                &line,
                &progress,
                prev_passed,
                prev_failed,
            );

            // Flush DB a intervalli (max 500ms tra UPDATE)
            if last_db_flush.elapsed() >= FLUSH_INTERVAL {
                flush_playwright_log(&db, job_id, &acc_log, &progress).await;
                last_db_flush = std::time::Instant::now();
            }
        }
    }

    // Flush finale (cattura le ultime righe sotto la soglia interval)
    flush_playwright_log(&db, job_id, &acc_log, &progress).await;

    (full_bytes, progress)
}

/// Emette l'evento Line (sempre) e Progress (solo se i contatori passed/failed
/// sono cambiati rispetto ai valori precedenti) per una riga di stdout.
fn emit_stdout_line_events(
    channels: &crate::playwright_live::PlaywrightChannels,
    job_id: Uuid,
    line: &str,
    progress: &crate::playwright_live::PlaywrightProgress,
    prev_passed: u32,
    prev_failed: u32,
) {
    crate::playwright_live::emit(
        channels,
        crate::playwright_live::PlaywrightEvent::Line {
            job_id,
            line: line.chars().take(2000).collect(),
        },
    );
    if progress.passed != prev_passed || progress.failed != prev_failed {
        crate::playwright_live::emit(
            channels,
            crate::playwright_live::PlaywrightEvent::Progress {
                job_id,
                progress: progress.clone(),
            },
        );
    }
}

/// UPDATE incrementale del log e del progress sul record `jobs` (best-effort).
async fn flush_playwright_log(
    db: &sqlx::PgPool,
    job_id: Uuid,
    acc_log: &str,
    progress: &crate::playwright_live::PlaywrightProgress,
) {
    let _ = sqlx::query("UPDATE jobs SET output_log = $1, progress = $2 WHERE id = $3")
        .bind(acc_log)
        .bind(serde_json::to_value(progress).unwrap_or(serde_json::json!({})))
        .bind(job_id)
        .execute(db)
        .await;
}

pub(super) async fn tool_run_playwright_tests(ctx: &AgentToolContext, input: &Value) -> String {
    // ── 1. Parametri ─────────────────────────────────────────────────────────
    let params = parse_playwright_params(input);
    let PlaywrightRunParams {
        filter,
        project_arg,
        workers,
        reporter,
        explicit_base_url,
        timeout,
        test_timeout_ms,
        auto_start,
        config_path_override,
        cleanup_stale,
    } = params;

    // ── 2. Controllo presenza Playwright ─────────────────────────────────────
    let (playwright_root, stale_configs) = match resolve_playwright_root(ctx, &config_path_override)
    {
        Ok(v) => v,
        Err(msg) => return msg,
    };
    let root = &playwright_root;
    tracing::info!(playwright_root = %root.display(), "run_playwright_tests: directory scelta");

    // Cleanup automatico di config "wrapper stale" alla radice (es. residuo di run precedenti)
    let cleanup_notes: Vec<String> = if cleanup_stale && ctx.can_write && !stale_configs.is_empty()
    {
        cleanup_stale_configs(&stale_configs)
    } else {
        Vec::new()
    };

    let has_config = root.join("playwright.config.ts").is_file()
        || root.join("playwright.config.js").is_file()
        || root.join("playwright.config.mjs").is_file();
    let has_playwright_nm = root
        .join("node_modules")
        .join("@playwright")
        .join("test")
        .is_dir();

    if !has_config && !has_playwright_nm {
        return format!(
            "[run_playwright_tests] Playwright non trovato nel progetto (cercato in {} e sottodirectory).\n\
             Installa con: run_command({{\"command\": \"pnpm add -D @playwright/test\", \"working_dir\": \"app\"}}).\n\
             Poi inizializza: run_command({{\"command\": \"npx playwright install --with-deps chromium\", \"working_dir\": \"app\"}}).",
            ctx.root_path.display()
        );
    }

    // ── 2bis. Pre-flight check librerie sistema chromium-headless-shell ──────
    if let Err(msg) = run_preflight_check(ctx, root).await {
        return msg;
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
    let base_url = determine_base_url(explicit_base_url, &port_rows).await;

    // BACKEND_API_URL: porta del servizio backend per il global-setup.ts
    // (seed utenti e health-check pre-test). Non override se già in env.
    let backend_api_url: Option<String> = {
        let dev_port = base_url.as_ref().and_then(|u| port_from_localhost_url(u));
        pick_backend_port(&port_rows, dev_port).map(|p| format!("http://127.0.0.1:{}", p))
    };

    // ── 5. Verifica se il server è raggiungibile; suggerisci avvio se no ─────
    let server_status = check_server_status(ctx, root, base_url.as_deref(), auto_start).await;

    // ── 6. Costruisci il comando Playwright ───────────────────────────────────
    let command_str =
        build_playwright_command(test_timeout_ms, workers, &reporter, &project_arg, &filter);
    tracing::info!(command = %command_str, root = %root.display(), "run_playwright_tests: avvio comando");

    // ── 7. Esegui con env BASE_URL ────────────────────────────────────────────
    let mut child = match spawn_playwright_child(
        &command_str,
        root,
        base_url.as_deref(),
        backend_api_url.as_deref(),
    ) {
        Ok(c) => c,
        Err(e) => return format!("[run_playwright_tests] Errore avvio processo: {e}"),
    };

    // ── Live monitoring: INSERT iniziale + broadcast channel ─────────────────
    // Separazione DB per-progetto: la tabella `jobs` e' migrata. Risolvo una
    // sola volta il pool del progetto (per ctx.project_id in scope) e lo riuso
    // per INSERT, UPDATE live nel task stdout e UPDATE finale.
    let proj_pool = crate::project_db_routes::project_data_pool_from(&ctx.db, ctx.project_id).await;
    let job_id = Uuid::new_v4();
    let _ = sqlx::query(
        "INSERT INTO jobs (id, project_id, kind, status, input, progress, output_log) \
         VALUES ($1, $2, 'playwright_test', 'running', $3, '{}'::jsonb, '')",
    )
    .bind(job_id)
    .bind(ctx.project_id)
    .bind(serde_json::json!({
        "label": "Run in corso...",
        "command": command_str,
        "started_at": chrono::Utc::now().to_rfc3339(),
    }))
    .execute(&proj_pool)
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
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();

    // Separazione DB per-progetto: il task di stdout aggiorna `jobs` (migrata),
    // quindi cattura il pool del progetto gia' risolto, non il meta-pool.
    let db_for_stdout = proj_pool.clone();
    let channels_for_stdout = ctx.playwright_channels.clone();
    let stdout_task = tokio::spawn(stream_playwright_stdout(
        stdout_handle,
        db_for_stdout,
        channels_for_stdout,
        job_id,
    ));
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut err) = stderr_handle {
            let _ = err.read_to_end(&mut buf).await;
        }
        buf
    });

    let timeout_result = tokio::time::timeout(Duration::from_secs(timeout), child.wait()).await;

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
    let (stdout_bytes, live_progress) = stdout_task.await.unwrap_or_else(|_| {
        (
            Vec::new(),
            crate::playwright_live::PlaywrightProgress::default(),
        )
    });
    let stderr_bytes = stderr_task.await.unwrap_or_default();
    let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
    let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();

    // ── 8. Parsa statistiche ──────────────────────────────────────────────────
    let stats = parse_playwright_output_stats(&stdout, &stderr);

    // ── 8b. Raccogli artifact (screenshot, video, trace) ─────────────────────
    let artifacts = collect_playwright_artifacts(root, &ctx.root_path);

    // ── 9. Finalizza il record `jobs` (UPDATE, non nuova INSERT) ───────────────
    finalize_playwright_job(
        ctx,
        &proj_pool,
        job_id,
        exit_code,
        &stats,
        &live_progress,
        &artifacts,
        &command_str,
    )
    .await;

    // ── 10. Output finale ─────────────────────────────────────────────────────
    format_playwright_run_output(
        root,
        &cleanup_notes,
        &port_rows,
        base_url.as_deref(),
        backend_api_url.as_deref(),
        &server_status,
        &command_str,
        exit_code,
        &stats,
        &stdout,
        &stderr,
    )
}

/// Costruisce il descrittore JSON dell'esito (label + message) da usare nel
/// record `jobs` e negli eventi. Ritorna (status, label, message).
fn playwright_result_summary(
    exit_code: i32,
    stats: &PlaywrightStats,
) -> (&'static str, String, String) {
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
            format!(
                ". Falliti: {}",
                stats
                    .failed_tests
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else {
            String::new()
        }
    );
    (status, label, msg)
}

/// Calcola il progress finale: privilegia le stats del parser completo, ma
/// preserva flaky/failed_specs accumulati live se disponibili.
fn build_final_progress(
    stats: &PlaywrightStats,
    live_progress: &crate::playwright_live::PlaywrightProgress,
) -> crate::playwright_live::PlaywrightProgress {
    crate::playwright_live::PlaywrightProgress {
        total: live_progress
            .total
            .or(Some((stats.passed + stats.failed + stats.skipped) as u32)),
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
    }
}

/// Finalizza il record `jobs` (UPDATE), emette gli eventi di esito ai pannelli
/// e programma il cleanup deferito del channel SSE (30s).
#[allow(clippy::too_many_arguments)]
async fn finalize_playwright_job(
    ctx: &AgentToolContext,
    proj_pool: &sqlx::PgPool,
    job_id: Uuid,
    exit_code: i32,
    stats: &PlaywrightStats,
    live_progress: &crate::playwright_live::PlaywrightProgress,
    artifacts: &[serde_json::Value],
    command_str: &str,
) {
    let (status, label, msg) = playwright_result_summary(exit_code, stats);
    let final_progress = build_final_progress(stats, live_progress);

    update_playwright_job_record(
        proj_pool,
        ctx.project_id,
        job_id,
        status,
        &label,
        &msg,
        artifacts,
        command_str,
        exit_code,
        &final_progress,
    )
    .await;

    emit_playwright_final_events(
        ctx,
        job_id,
        status,
        &label,
        &msg,
        artifacts,
        exit_code,
        final_progress,
    );
}

/// UPDATE del record `jobs` con esito finale (status, input, progress).
/// Separazione DB per-progetto: usa il pool del progetto gia' risolto.
#[allow(clippy::too_many_arguments)]
async fn update_playwright_job_record(
    proj_pool: &sqlx::PgPool,
    pid: Uuid,
    job_id: Uuid,
    status: &str,
    label: &str,
    msg: &str,
    artifacts: &[serde_json::Value],
    command_str: &str,
    exit_code: i32,
    final_progress: &crate::playwright_live::PlaywrightProgress,
) {
    match sqlx::query("UPDATE jobs SET status = $1, input = $2, progress = $3 WHERE id = $4")
        .bind(status)
        .bind(serde_json::json!({
            "label": label,
            "message": msg,
            "artifacts": artifacts,
            "command": command_str,
            "exit_code": exit_code,
        }))
        .bind(serde_json::to_value(final_progress).unwrap_or(serde_json::json!({})))
        .bind(job_id)
        .execute(proj_pool)
        .await
    {
        Ok(r) => {
            tracing::info!(rows = r.rows_affected(), project_id = %pid, status = %status, artifacts = artifacts.len(), "playwright_test job aggiornato")
        }
        Err(e) => {
            tracing::error!(error = %e, project_id = %pid, "playwright_test job UPDATE fallito")
        }
    }
}

/// Emette gli eventi di esito (dispatcher JobCreated + PlaywrightEvent::Final)
/// e programma il cleanup deferito del channel SSE (30s).
#[allow(clippy::too_many_arguments)]
fn emit_playwright_final_events(
    ctx: &AgentToolContext,
    job_id: Uuid,
    status: &str,
    label: &str,
    msg: &str,
    artifacts: &[serde_json::Value],
    exit_code: i32,
    final_progress: crate::playwright_live::PlaywrightProgress,
) {
    // Dispatcher: notifica esito finale → toast + highlight pannello Playwright
    nexus_events::dispatcher::emit(
        &ctx.project_channels,
        ctx.project_id,
        nexus_events::event::ProjectEvent::JobCreated {
            id: job_id,
            job_kind: "playwright_test".to_string(),
            status: status.to_string(),
            label: label.to_string(),
            summary: Some(msg.to_string()),
            artifacts: serde_json::to_value(artifacts).unwrap_or(serde_json::Value::Null),
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

/// Ultime `n` righe di `text` mantenendo l'ordine originale.
fn last_n_lines(text: &str, n: usize) -> String {
    text.lines()
        .rev()
        .take(n)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

/// Prime `n` righe non vuote di `text`.
fn first_n_nonempty_lines(text: &str, n: usize) -> String {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .take(n)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Rappresentazione compatta delle porte allocate (`:PORT (label)`).
fn format_port_info(port_rows: &[(i32, String)]) -> String {
    port_rows
        .iter()
        .map(|(p, l)| {
            if l.is_empty() {
                format!(":{}", p)
            } else {
                format!(":{} ({})", p, l)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Formatta il report testuale finale del run Playwright per il tool_result.
#[allow(clippy::too_many_arguments)]
fn format_playwright_run_output(
    root: &Path,
    cleanup_notes: &[String],
    port_rows: &[(i32, String)],
    base_url: Option<&str>,
    backend_api_url: Option<&str>,
    server_status: &str,
    command_str: &str,
    exit_code: i32,
    stats: &PlaywrightStats,
    stdout: &str,
    stderr: &str,
) -> String {
    let stdout_tail = last_n_lines(stdout, 60);
    let stderr_excerpt = first_n_nonempty_lines(stderr, 20);

    let status_label = if exit_code == 0 {
        "TUTTI I TEST PASSATI"
    } else {
        "TEST FALLITI"
    };
    let port_info = format_port_info(port_rows);

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
        port_info = if port_info.is_empty() {
            "nessuna porta allocata".to_string()
        } else {
            port_info
        },
        base_url_display = base_url.unwrap_or("(da playwright.config.ts)"),
        backend_api_url_display = backend_api_url
            .unwrap_or("(non trovata — verifica label 'backend-*' in nexus_port_allocations)"),
        server_status = server_status,
        command_str = command_str,
        passed = stats.passed,
        failed = stats.failed,
        skipped = stats.skipped,
        total = stats.passed + stats.failed + stats.skipped,
        failed_list = if stats.failed_tests.is_empty() {
            String::new()
        } else {
            format!(
                "Test falliti:\n{}",
                stats
                    .failed_tests
                    .iter()
                    .map(|t| format!("  - {t}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
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

/// Risolve la working directory relativa alla root del progetto. Ritorna
/// direttamente la stringa d'errore gia' formattata (pronta per il return del
/// tool) se il path e' fuori dalla root o invalido. Punto unico condiviso dai
/// tool testing/lint (regola L).
fn resolve_work_path(
    ctx: &AgentToolContext,
    working_dir: &str,
) -> Result<std::path::PathBuf, String> {
    if working_dir.is_empty() {
        return Ok(ctx.root_path.clone());
    }
    match resolve_relative_path(&ctx.root_path, working_dir) {
        Ok(p) => Ok(p),
        Err(e) => Err(format!(
            "[Errore percorso: {}]",
            e.1["error"].as_str().unwrap_or("path error")
        )),
    }
}

/// Rileva il comando di test per il framework presente in `work_path`
/// (cargo/vitest/jest/pnpm/pytest/mix/go). Ritorna None se nessun marker noto.
fn detect_test_command(work_path: &Path, test_name: &str) -> Option<String> {
    if work_path.join("Cargo.toml").is_file() {
        return Some(format!("cargo test {} -- --nocapture 2>&1", test_name));
    }
    if work_path.join("package.json").is_file() {
        // Node: pnpm/npm test con filtro
        let cmd = if work_path.join("vitest.config.ts").is_file()
            || work_path.join("vitest.config.js").is_file()
        {
            format!("npx vitest run -t '{}' 2>&1", test_name)
        } else if work_path.join("jest.config.ts").is_file()
            || work_path.join("jest.config.js").is_file()
        {
            format!("npx jest -t '{}' 2>&1", test_name)
        } else {
            format!("pnpm test -- --grep '{}' 2>&1", test_name)
        };
        return Some(cmd);
    }
    if work_path.join("pytest.ini").is_file()
        || work_path.join("pyproject.toml").is_file()
        || work_path.join("setup.py").is_file()
    {
        return Some(format!("python -m pytest -k '{}' -v 2>&1", test_name));
    }
    if work_path.join("mix.exs").is_file() {
        return Some(format!("mix test --only {} 2>&1", test_name));
    }
    if work_path.join("go.mod").is_file() {
        return Some(format!("go test -run '{}' -v ./... 2>&1", test_name));
    }
    None
}

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

    let work_path = match resolve_work_path(ctx, working_dir) {
        Ok(p) => p,
        Err(e) => return e,
    };

    let command = match detect_test_command(&work_path, test_name) {
        Some(c) => c,
        None => {
            return format!(
                "[Errore: framework di test non rilevato in '{}'. \
                 File cercati: Cargo.toml, package.json, pytest.ini, pyproject.toml, mix.exs, go.mod]",
                work_path.display()
            )
        }
    };

    run_test_command(ctx, &command, &work_path, timeout_secs).await
}

/// Rileva il comando di lint per il linter presente in `work_path`
/// (clippy/eslint/ruff), rispettando `check_only`. None se nessun linter noto.
fn detect_lint_command(work_path: &Path, check_only: bool) -> Option<String> {
    if work_path.join("Cargo.toml").is_file() {
        let cmd = if check_only {
            "cargo clippy --all-targets -- -D warnings 2>&1"
        } else {
            "cargo clippy --fix --allow-dirty --allow-staged --all-targets 2>&1"
        };
        return Some(cmd.to_string());
    }
    if work_path.join("package.json").is_file() {
        let cmd = if check_only {
            "npx eslint . 2>&1"
        } else {
            "npx eslint . --fix 2>&1"
        };
        return Some(cmd.to_string());
    }
    if work_path.join("pyproject.toml").is_file()
        || work_path.join("setup.py").is_file()
        || work_path.join("ruff.toml").is_file()
    {
        let cmd = if check_only {
            "ruff check . 2>&1"
        } else {
            "ruff check . --fix 2>&1"
        };
        return Some(cmd.to_string());
    }
    None
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

    let work_path = match resolve_work_path(ctx, working_dir) {
        Ok(p) => p,
        Err(e) => return e,
    };

    let command = match detect_lint_command(&work_path, check_only) {
        Some(c) => c,
        None => {
            return format!(
                "[Errore: linter non rilevato in '{}'. \
                 Supportati: cargo clippy (Rust), eslint (Node), ruff (Python)]",
                work_path.display()
            )
        }
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
        Err(e) => {
            return format!(
                "[Errore percorso: {}]",
                e.1["error"].as_str().unwrap_or("path error")
            )
        }
    };
    if !target.is_file() {
        return format!("[Errore: '{}' non e' un file]", path_str);
    }

    let ext = target
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let command = match detect_format_command(&ext, &target, check_only) {
        Some(c) => c,
        None => {
            return format!(
                "[Errore: formatter non disponibile per estensione '.{}'. \
                 Supportati: .rs (rustfmt), .ts/.js/.json/.css/.md (prettier), .py (black), .go (gofmt)]",
                ext
            )
        }
    };

    run_test_command(ctx, &command, &ctx.root_path, 30).await
}

/// Seleziona il comando di formattazione in base all'estensione del file
/// (rustfmt/prettier/black/gofmt), rispettando `check_only`. None se non
/// supportata. Punto unico della mappa estensione->formatter (regola L).
fn detect_format_command(ext: &str, target: &Path, check_only: bool) -> Option<String> {
    let cmd = match ext {
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
        _ => return None,
    };
    Some(cmd)
}

/// Helper comune: esegue un comando con timeout e cattura output.
async fn run_test_command(
    _ctx: &AgentToolContext,
    command: &str,
    work_dir: &Path,
    timeout_secs: u64,
) -> String {
    let (exit_code, stdout, stderr) =
        match spawn_and_capture_output(command, work_dir, timeout_secs).await {
            Ok(v) => v,
            Err(msg) => return msg,
        };

    // Tronca output se troppo lungo (coda degli ultimi 6000 byte)
    let stdout_tail = truncate_output_tail(stdout, 6000);
    let stderr_tail = truncate_output_tail(stderr, 6000);

    let mut result = format!("Exit code: {}\n", exit_code);
    if !stdout_tail.is_empty() {
        result.push_str(&format!("\nOutput:\n{}", stdout_tail));
    }
    if !stderr_tail.is_empty() {
        result.push_str(&format!("\nErrori:\n{}", stderr_tail));
    }
    result
}

/// Lancia `command` nella shell isolata, cattura stdout/stderr in parallelo a
/// `child.wait()` (evita deadlock del buffer pipe ~64 KB) con timeout. Ritorna
/// (exit_code, stdout, stderr) oppure Err con il messaggio d'errore pronto.
async fn spawn_and_capture_output(
    command: &str,
    work_dir: &Path,
    timeout_secs: u64,
) -> Result<(i32, String, String), String> {
    use tokio::io::AsyncReadExt;

    // L'isolamento env (env_clear + host env filtrato) e' dentro
    // isolated_command, punto unico (regola L).
    let mut child = match crate::sandbox::isolated_command(&crate::sandbox::agent_shell())
        .arg("-c")
        .arg(command)
        .current_dir(work_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Err(format!("[Errore avvio: {}]", e)),
    };

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

    let timeout_result =
        tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait()).await;

    let exit_code = match timeout_result {
        Ok(Ok(status)) => status.code().unwrap_or(-1),
        Ok(Err(e)) => return Err(format!("[Errore attesa processo: {}]", e)),
        Err(_) => {
            let _ = child.start_kill();
            return Err(format!(
                "[Timeout dopo {}s. Comando: {}]",
                timeout_secs, command
            ));
        }
    };

    let stdout_bytes = stdout_task.await.unwrap_or_default();
    let stderr_bytes = stderr_task.await.unwrap_or_default();
    let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
    let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();
    Ok((exit_code, stdout, stderr))
}

/// Tronca `text` mantenendo la coda degli ultimi `max_out` byte, prefissando un
/// marcatore. Sotto soglia lo restituisce invariato.
fn truncate_output_tail(text: String, max_out: usize) -> String {
    if text.len() > max_out {
        format!("...(troncato)\n{}", &text[text.len() - max_out..])
    } else {
        text
    }
}
