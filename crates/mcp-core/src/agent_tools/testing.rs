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
use crate::suite_verification::{SuiteOutcome, SuiteStats};
use nexus_types::tool_outcome::RispostaTool;
use std::time::Duration;
use tokio::io::AsyncReadExt;

/// Tetto di default di una esecuzione di suite: lo stesso numero che il punto
/// unico impone come pavimento a chi delega da un contesto piu' stretto (il
/// final gate). Due costanti separate sarebbero divergite alla prima modifica.
const PLAYWRIGHT_DEFAULT_TIMEOUT: u64 = crate::suite_verification::TIMEOUT_SUITE_DEFAULT_S;
const PLAYWRIGHT_MAX_TIMEOUT: u64 = 900;

/// Finestra massima (secondi) in cui il runner attende che il servizio
/// bersaglio della suite risponda stabilmente PRIMA di lanciare i test.
/// Setting DB (regola G), default veicolato dalla migrazione 0662.
const PLAYWRIGHT_READINESS_KEY: &str = "agent.playwright.readiness_timeout_seconds";
/// Ripiego se il setting manca dal DB: la migrazione 0662 lo veicola, ma un DB
/// non ancora migrato non deve trasformare il gate in un blocco infinito.
const DEFAULT_TARGET_READINESS_SECONDS: u64 = 60;

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
    /// Argomenti della riga di comando quando la suite arriva qui delegata da
    /// un tool generico (vedi [`esegui_suite_delegata`]). NON e' esposto nel
    /// catalogo dei tool: e' il canale con cui `run_command`/`run_tests`
    /// consegnano all'esecutore unico cio' che l'agente aveva scritto, senza
    /// che nulla vada perso per strada.
    extra_args: Vec<String>,
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
        extra_args: extra_args_da_input(input),
    }
}

/// Argomenti passati dalla delega di un tool generico: lista di stringhe, gli
/// altri tipi ignorati (il canale e' interno, ma un `null` in mezzo non deve
/// diventare un argomento vuoto sulla riga di comando).
fn extra_args_da_input(input: &Value) -> Vec<String> {
    input
        .get("extra_args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Risolve la playwright root e la lista di config wrapper stale. Se
/// `config_path_override` e' passato, lo risolve (bloccando il traversal);
/// altrimenti delega a `pick_playwright_root_with_stale`. In caso di
/// config_path invalido ritorna Err con il messaggio pronto per il return.
fn resolve_playwright_root(
    ctx: &AgentToolContext,
    config_path_override: &Option<String>,
) -> Result<(std::path::PathBuf, Vec<std::path::PathBuf>), RispostaTool> {
    let Some(cp) = config_path_override else {
        return Ok(pick_playwright_root_with_stale(&ctx.root_path));
    };
    // Punto unico (regola L): de-duplica la root se l'agente l'ha inclusa
    // in config_path e blocca il traversal (resolve_relative_path).
    // RIMEDIABILE, e prima usciva NUDO: un `config_path` sbagliato tornava
    // all'agente come un'esecuzione riuscita che non aveva eseguito nulla.
    let not_found = || {
        RispostaTool::fallito_rimediabile(format!(
            "[run_playwright_tests] config_path '{cp}' non trovato. Passa una directory relativa (es. \"app\") o un file config."
        ))
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
async fn run_preflight_check(ctx: &AgentToolContext, root: &Path) -> Result<(), RispostaTool> {
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
    // DEL SISTEMA: mancano librerie di sistema, e le tre strade che il
    // messaggio propone richiedono tutte privilegi che questo run non ha (Sudo
    // Manager, apt-get, install-deps). Ripetere la chiamata non ne installa
    // nessuna. Anche questo usciva nudo, cioe' come un successo.
    Err(RispostaTool::fallito_di_sistema(preflight_blocked_message(
        &missing, root,
    )))
}

/// Estrae la porta numerica da una URL `http(s)://localhost:PORT/...`.
pub(crate) fn port_from_localhost_url(url: &str) -> Option<i32> {
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
            svc_result.testo.chars().take(200).collect::<String>()
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

/// Gate di readiness del bersaglio della suite (regola L: delega al contratto
/// della remediation — `service_recovery::await_port_ready`, cioe' `probe_port`
/// + `stable_enough`): `Some(causa)` se la porta della BASE_URL appartiene a
/// una unit di servizio del progetto e NON risponde stabilmente entro la
/// finestra. `None` = pronta, oppure nessun contratto da attendere.
///
/// Il gate scatta SOLO se la porta e' legata a una unit
/// (`nexus_port_allocations.service_unit`, lo stesso criterio con cui
/// l'observer apre le diagnosi): senza unit non c'e' alcun servizio che DEBBA
/// rispondere prima del lancio — e' il caso del `webServer` avviato dalla
/// suite stessa, la cui porta risponde solo DOPO, e un'attesa qui lo
/// bloccherebbe sempre.
/// `pub(crate)`: e' il PUNTO UNICO della readiness del bersaglio prima di una
/// verifica che lo interroga — lo riusano il runner Playwright e le sonde
/// `http` del criteria_runner (GAP-6: un probe a servizio freddo produceva
/// `Failed` su codice sano, 31 rossi su 53 giri fabbricati dal cold start).
pub(crate) async fn await_target_service_ready(
    db: &sqlx::PgPool,
    project_id: Uuid,
    base_url: Option<&str>,
) -> Option<String> {
    let port = base_url.and_then(port_from_localhost_url)?;
    let unit = target_service_unit(db, project_id, port).await?;
    let readiness = playwright_readiness_window(db).await;
    let ready = crate::project_workspace::service_recovery::await_port_ready(
        u16::try_from(port).ok()?,
        readiness,
    )
    .await;
    if ready.ready() {
        tracing::info!(port, unit = %unit, "run_playwright_tests: bersaglio pronto (risposta stabile)");
        return None;
    }
    Some(target_not_ready_cause(&ready, port, &unit, readiness))
}

/// La unit di servizio a cui il registro lega la porta bersaglio, se esiste.
async fn target_service_unit(db: &sqlx::PgPool, project_id: Uuid, port: i32) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT service_unit FROM nexus_port_allocations WHERE project_id = $1 AND port = $2",
    )
    .bind(project_id)
    .bind(port)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .flatten()
}

/// Finestra di readiness del bersaglio dal DB (regola G):
/// `agent.playwright.readiness_timeout_seconds`. `parse::<u64>` rifiuta da se'
/// i valori negativi; `0` riduce il gate alla sola finestra di stabilita'.
async fn playwright_readiness_window(db: &sqlx::PgPool) -> Duration {
    let secs = crate::settings::get_setting(db, PLAYWRIGHT_READINESS_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_TARGET_READINESS_SECONDS);
    Duration::from_secs(secs)
}

/// La causa del setup_failed per bersaglio non pronto: UNA riga (e' quella che
/// `extract_failure_cause` raccogliera' come `failure_cause` del job, taglio a
/// 300 char) con porta, unit, finestra attesa e cosa ha risposto l'ULTIMA
/// osservazione.
///
/// Il numero e' dichiarato per cio' che e' — la finestra CONFIGURATA — non come
/// durata dell'attesa: quella e' la finestra piu' la stabilita' pretesa dal
/// contratto, e un numero senza la sua premessa e' un'opinione (regola O).
fn target_not_ready_cause(
    ready: &crate::project_workspace::service_recovery::PortReadiness,
    port: i32,
    unit: &str,
    readiness: Duration,
) -> String {
    format!(
        "Servizio non pronto: la porta {port} (unit {unit}) non risponde stabilmente \
         entro la finestra configurata di {}s (ultima osservazione: {})",
        readiness.as_secs(),
        ready.answer.describe(),
    )
}

/// Costruisce la riga di comando `npx playwright test ...` con timeout, workers,
/// reporter e gli argomenti opzionali project/filter.
///
/// `extra_args` sono gli argomenti scritti dall'agente quando la suite arriva
/// delegata da un tool generico: vengono appesi COSI' COME SONO (ri-quotati),
/// e i default omologhi non vengono aggiunti — passare `--workers 4` e trovarsi
/// `--workers 1 --workers 4` significherebbe che il valore vincente dipende
/// dall'ordine, cioe' da un dettaglio che nessun chiamante controlla.
fn build_playwright_command(
    test_timeout_ms: u64,
    workers: u64,
    reporter: &str,
    project_arg: &Option<String>,
    filter: &Option<String>,
    extra_args: &[String],
) -> String {
    let mut cmd_parts = vec![
        "npx".to_string(),
        "playwright".to_string(),
        "test".to_string(),
    ];
    if !flag_presente(extra_args, "--timeout") {
        cmd_parts.push("--timeout".to_string());
        cmd_parts.push(test_timeout_ms.to_string());
    }
    const FLAG_WORKERS: &str = "--workers";
    const FLAG_REPORTER: &str = "--reporter";
    if !flag_presente(extra_args, FLAG_WORKERS) && !flag_presente(extra_args, "-j") {
        cmd_parts.push(FLAG_WORKERS.to_string());
        cmd_parts.push(workers.to_string());
    }
    if !flag_presente(extra_args, FLAG_REPORTER) {
        cmd_parts.push(FLAG_REPORTER.to_string());
        cmd_parts.push(reporter.to_string());
    }
    if let Some(p) = project_arg {
        if !flag_presente(extra_args, "--project") {
            cmd_parts.push("--project".to_string());
            cmd_parts.push(p.clone());
        }
    }
    if let Some(f) = filter {
        cmd_parts.push(f.clone());
    }
    cmd_parts.extend(extra_args.iter().map(|a| quota_argomento(a)));
    cmd_parts.join(" ")
}

/// `flag` presente fra gli argomenti, sia staccato (`--workers 4`) sia
/// attaccato (`--workers=4`).
fn flag_presente(args: &[String], flag: &str) -> bool {
    args.iter()
        .any(|a| a == flag || a.starts_with(&format!("{flag}=")))
}

/// Ri-quota un argomento per la shell: la riga costruita qui torna a essere
/// UNA stringa passata a `sh -c`, quindi un argomento che conteneva spazi
/// (`--grep "login utente"`) si spezzerebbe in due senza questo passaggio.
fn quota_argomento(arg: &str) -> String {
    let sicuro = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_=./:@,+".contains(c));
    if sicuro {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', r"'\''"))
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

/// Contesto minimo per ESEGUIRE una suite, costruibile sia dal ctx dei tool
/// (percorso dell'agente) sia dall'adapter dei criteri del final gate.
///
/// E' cio' che ha reso possibile far convergere i tre esecutori su UN solo
/// runner (regola L): prima il gate lanciava la suite dal tool `run_command`
/// generico — senza `BASE_URL` dalle porte allocate, senza riga `jobs`, senza
/// nulla che potesse riconoscere l'esecuzione dell'agente — e l'agente la
/// lanciava da qui. Due esecuzioni della stessa suite che non si vedevano.
#[derive(Clone)]
pub(crate) struct SuiteEnv {
    /// Pool META: porte allocate, settings, risoluzione del pool di progetto.
    pub meta_db: sqlx::PgPool,
    pub project_id: Uuid,
    /// Radice su cui la suite gira: la playwright root per il tool, la radice
    /// del run per il gate.
    pub root: std::path::PathBuf,
    /// Canali live: `None` fuori dal percorso dei tool (il gate non ha un
    /// consumatore SSE agganciato). La riga `jobs` si scrive comunque — e' il
    /// registro, non la diretta.
    pub playwright_channels: Option<crate::playwright_live::PlaywrightChannels>,
    pub project_channels: Option<nexus_events::ProjectChannels>,
}

impl SuiteEnv {
    /// Dal contesto dei tool (percorso agente).
    pub(crate) fn dal_ctx(ctx: &AgentToolContext, root: std::path::PathBuf) -> Self {
        Self {
            meta_db: (*ctx.db).clone(),
            project_id: ctx.project_id,
            root,
            playwright_channels: Some(ctx.playwright_channels.clone()),
            project_channels: Some(ctx.project_channels.clone()),
        }
    }
}

/// L'UNICO esecutore reale di una suite Playwright (impl di
/// [`crate::suite_verification::SuiteExecutor`]).
pub(crate) struct PlaywrightProcessExecutor {
    env: SuiteEnv,
}

impl PlaywrightProcessExecutor {
    pub(crate) fn new(env: SuiteEnv) -> Self {
        Self { env }
    }
}

#[async_trait::async_trait]
impl crate::suite_verification::SuiteExecutor for PlaywrightProcessExecutor {
    async fn esegui(
        &self,
        inv: &crate::suite_verification::SuiteInvocation,
    ) -> Result<crate::suite_verification::SuiteRun, String> {
        esegui_processo_suite(&self.env, inv).await
    }
}

/// Annuncia ai pannelli l'esito FINALE quando la classificazione ha spostato
/// cio' che il runner aveva misurato (l'unico caso reale: `flaky`). Senza,
/// l'ultimo evento emesso resterebbe quello del runner — "test falliti" — e il
/// pannello mostrerebbe rosso un run gia' riconosciuto instabile.
struct AnnuncioAiPannelli {
    env: SuiteEnv,
}

#[async_trait::async_trait]
impl crate::suite_verification::EsitoAnnunciato for AnnuncioAiPannelli {
    async fn annuncia(
        &self,
        job_id: Uuid,
        outcome: crate::suite_verification::SuiteOutcome,
        test_instabili: &[String],
    ) {
        let Some(pc) = &self.env.project_channels else {
            return;
        };
        let label = if test_instabili.is_empty() {
            "Test instabili (flaky)".to_string()
        } else {
            format!("{} test instabili (flaky)", test_instabili.len())
        };
        nexus_events::dispatcher::emit(
            pc,
            self.env.project_id,
            nexus_events::event::ProjectEvent::JobCreated {
                id: job_id,
                job_kind: "playwright_test".to_string(),
                status: outcome.job_status().to_string(),
                label,
                summary: Some(
                    "Falliti alla prima esecuzione, ripassati alla riesecuzione mirata a \
                     codice invariato: debito di test, non difetto dell'applicazione."
                        .to_string(),
                ),
                artifacts: serde_json::Value::Null,
            },
        );
    }
}

/// Costruisce le dipendenze della verifica (esecutore + memoria + chiave +
/// policy). Punto unico della composizione: i chiamanti non scelgono quale
/// memoria o quale chiave usare, le ricevono.
///
/// `root_chiave` e' distinta da `env.root` di proposito: si ESEGUE dove sta la
/// configurazione Playwright (che puo' essere una sottodirectory), ma si
/// CHIAVE sul codice che i test esercitano, cioe' la radice del run. Una chiave
/// calcolata sulla sola `app/` riuserebbe l'esito dopo una modifica al backend
/// che sta accanto.
pub(crate) async fn suite_deps(
    env: SuiteEnv,
    root_chiave: std::path::PathBuf,
) -> crate::suite_verification::SuiteDeps {
    use crate::suite_verification::{state_key::ChiaveAlberoEServizi, SuiteDeps, SuitePolicy};

    let policy = SuitePolicy::dal_db(&env.meta_db).await;
    // La memoria vive nel DB del PROGETTO (tabella `jobs`): se non e'
    // raggiungibile la verifica prosegue senza memoria — riesegue, che e' il
    // comportamento sicuro — invece di fallire.
    let memo: Option<std::sync::Arc<dyn crate::suite_verification::SuiteMemo>> =
        match crate::project_db_routes::project_data_pool_from(&env.meta_db, env.project_id).await {
            Ok(pool) => Some(std::sync::Arc::new(
                crate::suite_verification::memo::PgSuiteMemo::new(pool, env.project_id),
            )),
            Err(e) => {
                tracing::warn!(
                    project_id = %env.project_id,
                    error = %e,
                    "verifica suite: DB del progetto non disponibile, nessuna memoria degli esiti"
                );
                None
            }
        };
    let chiave = std::sync::Arc::new(ChiaveAlberoEServizi::new(
        root_chiave,
        env.meta_db.clone(),
        env.project_id,
    ));
    let annuncio: Option<std::sync::Arc<dyn crate::suite_verification::EsitoAnnunciato>> =
        env.project_channels.as_ref().map(|_| {
            std::sync::Arc::new(AnnuncioAiPannelli { env: env.clone() })
                as std::sync::Arc<dyn crate::suite_verification::EsitoAnnunciato>
        });
    SuiteDeps {
        executor: std::sync::Arc::new(PlaywrightProcessExecutor::new(env)),
        memo,
        chiave,
        policy,
        annuncio,
    }
}

/// Ambiente della suite: porte allocate al progetto, BASE_URL del dev server e
/// BACKEND_API_URL per il global-setup. E' la parte che il gate non aveva
/// quando lanciava la suite dal tool `run_command` generico — e una suite E2E
/// che non sa a quale porta bussare fallisce per ragioni proprie.
async fn ambiente_della_suite(env: &SuiteEnv) -> AmbienteSuite {
    let port_rows: Vec<(i32, String)> = sqlx::query_as(
        "SELECT port, label FROM nexus_port_allocations WHERE project_id = $1 ORDER BY port ASC",
    )
    .bind(env.project_id)
    .fetch_all(&env.meta_db)
    .await
    .unwrap_or_default();

    let base_url = determine_base_url(None, &port_rows).await;
    let backend_api_url = {
        let dev_port = base_url.as_ref().and_then(|u| port_from_localhost_url(u));
        pick_backend_port(&port_rows, dev_port).map(|p| format!("http://127.0.0.1:{}", p))
    };
    AmbienteSuite {
        port_rows,
        base_url,
        backend_api_url,
    }
}

/// Porte del progetto e URL iniettati alla suite.
struct AmbienteSuite {
    port_rows: Vec<(i32, String)>,
    base_url: Option<String>,
    backend_api_url: Option<String>,
}

/// Cio' che il processo ha prodotto, prima di ogni interpretazione.
struct OutputProcesso {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    live_progress: crate::playwright_live::PlaywrightProgress,
}

/// Apre la riga `jobs` dell'esecuzione e il canale live. La riesecuzione mirata
/// ha una sua riga ed e' etichettata come tale: e' un'esecuzione reale e
/// nasconderla renderebbe il pannello incoerente col numero di run avvenuti —
/// ma non porta chiave, quindi la memoria non la puo' scambiare per l'esito di
/// una suite.
async fn apri_job_della_suite(
    env: &SuiteEnv,
    inv: &crate::suite_verification::SuiteInvocation,
    proj_pool: &sqlx::PgPool,
) -> (
    Uuid,
    crate::playwright_live::PlaywrightChannels,
    tokio::sync::broadcast::Sender<crate::playwright_live::PlaywrightEvent>,
) {
    use crate::suite_verification::ScopoEsecuzione;

    let job_id = Uuid::new_v4();
    let (etichetta, scopo) = match inv.scopo {
        ScopoEsecuzione::Suite => ("Run in corso...", "suite"),
        ScopoEsecuzione::RiesecuzioneMirata => (
            "Riesecuzione mirata (classificazione flaky)...",
            "targeted_rerun",
        ),
    };
    let _ = sqlx::query(
        "INSERT INTO jobs (id, project_id, kind, status, input, progress, output_log) \
         VALUES ($1, $2, 'playwright_test', 'running', $3, '{}'::jsonb, '')",
    )
    .bind(job_id)
    .bind(env.project_id)
    .bind(serde_json::json!({
        "label": etichetta,
        "command": inv.command,
        "started_at": chrono::Utc::now().to_rfc3339(),
        "scopo": scopo,
    }))
    .execute(proj_pool)
    .await;

    let channels = env
        .playwright_channels
        .clone()
        .unwrap_or_else(crate::playwright_live::new_channels);
    let live_tx = crate::playwright_live::register(&channels, job_id);
    annuncia_job_avviato(env, job_id, etichetta);
    (job_id, channels, live_tx)
}

/// Notifica al pannello che una esecuzione e' partita (la lista si aggiorna
/// subito, senza attendere l'esito).
fn annuncia_job_avviato(env: &SuiteEnv, job_id: Uuid, etichetta: &str) {
    let Some(pc) = &env.project_channels else {
        return;
    };
    nexus_events::dispatcher::emit(
        pc,
        env.project_id,
        nexus_events::event::ProjectEvent::JobCreated {
            id: job_id,
            job_kind: "playwright_test".to_string(),
            status: "running".to_string(),
            label: etichetta.to_string(),
            summary: None,
            artifacts: serde_json::Value::Null,
        },
    );
}

/// Esegue UNA volta il processo della suite: ambiente (BASE_URL/BACKEND_API_URL
/// dalle porte allocate), riga `jobs` con progresso live, parsing dei conteggi,
/// artefatti. NON classifica e non consulta la memoria: quelle sono decisioni
/// del punto unico [`crate::suite_verification`].
async fn esegui_processo_suite(
    env: &SuiteEnv,
    inv: &crate::suite_verification::SuiteInvocation,
) -> Result<crate::suite_verification::SuiteRun, String> {
    let root = radice_di_esecuzione(env, inv)?;

    // Separazione DB per-progetto: il pool va risolto PRIMA dello spawn (nessun
    // child orfano non monitorato).
    let proj_pool =
        crate::project_db_routes::project_data_pool_from(&env.meta_db, env.project_id)
            .await
            .map_err(|e| format!("DB del progetto non disponibile: {e}"))?;

    let amb = ambiente_della_suite(env).await;

    // ── Readiness del servizio bersaglio ─────────────────────────────────────
    // Una suite lanciata a t+0 da un riavvio trova una porta che risponde ma un
    // servizio ancora freddo (Vite che ritrasforma le dipendenze): il giro e'
    // destinato al rosso flaky, e i suoi rossi fabbricano cicli di correzione
    // su codice sano. Se il bersaglio non risponde stabilmente entro la
    // finestra, l'esito e' un setup_failed con la causa — la suite NON parte.
    // La memoria della verifica non resta avvelenata dal servizio giu': la
    // chiave di stato include la generazione dei servizi vivi, e il riavvio
    // del servizio la invalida da se'.
    if let Some(cause) =
        await_target_service_ready(&env.meta_db, env.project_id, amb.base_url.as_deref()).await
    {
        return Ok(setup_failed_bersaglio_non_pronto(env, inv, &proj_pool, &cause).await);
    }

    corsa_del_processo(env, inv, &root, &amb, &proj_pool).await
}

/// La corsa vera e propria: spawn del processo, job registrato, attesa con
/// monitoraggio live, statistiche, finalize e report. Separata
/// dall'orchestrazione (pool, ambiente, gate di readiness) che sta in
/// [`esegui_processo_suite`].
async fn corsa_del_processo(
    env: &SuiteEnv,
    inv: &crate::suite_verification::SuiteInvocation,
    root: &std::path::Path,
    amb: &AmbienteSuite,
    proj_pool: &sqlx::PgPool,
) -> Result<crate::suite_verification::SuiteRun, String> {
    use crate::suite_verification::SuiteRun;

    tracing::info!(
        command = %inv.command,
        root = %root.display(),
        scopo = ?inv.scopo,
        "verifica suite: avvio comando"
    );

    let mut child = spawn_playwright_child(
        &inv.command,
        root,
        amb.base_url.as_deref(),
        amb.backend_api_url.as_deref(),
    )
    .map_err(|e| format!("errore avvio processo: {e}"))?;

    let (job_id, channels, _live_tx) = apri_job_della_suite(env, inv, proj_pool).await;
    let out =
        attendi_processo_suite(&mut child, proj_pool, job_id, &channels, inv.timeout_s).await?;

    let stats = parse_playwright_output_stats(&out.stdout, &out.stderr);
    finalize_playwright_job(
        env,
        proj_pool,
        job_id,
        out.exit_code,
        &stats,
        &out.live_progress,
        &collect_playwright_artifacts(root, &env.root),
        &inv.command,
        &out.stdout,
        &out.stderr,
    )
    .await;

    Ok(SuiteRun {
        exit_code: out.exit_code,
        testo: format_playwright_run_output(root, amb, &inv.command, out.exit_code, &stats, &out),
        stats,
        job_id: Some(job_id),
    })
}

/// Chiude la corsa SENZA lanciare la suite quando il bersaglio non e' pronto:
/// registra il job (punto unico [`apri_job_della_suite`]) e lo finalizza lungo
/// la stessa catena dell'esito reale (regola O): exit -1 e zero test eseguiti
/// => `classifica_esito` lo dice setup_failed, e la causa viaggia nel canale
/// che `extract_failure_cause` legge.
async fn setup_failed_bersaglio_non_pronto(
    env: &SuiteEnv,
    inv: &crate::suite_verification::SuiteInvocation,
    proj_pool: &sqlx::PgPool,
    cause: &str,
) -> crate::suite_verification::SuiteRun {
    let (job_id, _channels, _live_tx) = apri_job_della_suite(env, inv, proj_pool).await;
    finalize_playwright_job(
        env,
        proj_pool,
        job_id,
        Some(-1),
        &SuiteStats::default(),
        &crate::playwright_live::PlaywrightProgress::default(),
        &[],
        &inv.command,
        "",
        cause,
    )
    .await;
    crate::suite_verification::SuiteRun {
        exit_code: Some(-1),
        stats: SuiteStats::default(),
        testo: format!(
            "[run_playwright_tests] Setup fallito, suite NON lanciata: {cause}.\n\
             Un giro di test partito ora produrrebbe rossi flaky su codice sano.\n\
             Verifica il servizio dal pannello Servizi (o riavvialo con \
             run_service), attendi che risponda e rilancia i test."
        ),
        job_id: Some(job_id),
    }
}

/// Radice su cui l'invocazione gira: la `working_dir` risolta sotto la radice
/// del run (il traversal e' bloccato da `resolve_relative_path`), o la radice
/// stessa.
fn radice_di_esecuzione(
    env: &SuiteEnv,
    inv: &crate::suite_verification::SuiteInvocation,
) -> Result<std::path::PathBuf, String> {
    match inv.working_dir.as_deref() {
        Some(wd) if !wd.is_empty() => resolve_relative_path(&env.root, wd)
            .map_err(|_| format!("working_dir '{wd}' fuori dalla radice del run")),
        _ => Ok(env.root.clone()),
    }
}

/// Attende il processo drenando stdout (parsing live + flush del log) e stderr
/// IN PARALLELO a `child.wait()`: senza, la pipe si riempie (~64 KB) e l'attesa
/// non ritorna mai. Ritorna `(exit_code, stdout, stderr, progresso live)`.
///
/// Un'attesa che non si conclude CHIUDE la riga `jobs`: prima restava `running`
/// per sempre, e il pannello mostrava come suite ancora in corso un processo
/// gia' ucciso.
async fn attendi_processo_suite(
    child: &mut tokio::process::Child,
    proj_pool: &sqlx::PgPool,
    job_id: Uuid,
    channels: &crate::playwright_live::PlaywrightChannels,
    timeout_s: u64,
) -> Result<OutputProcesso, String> {
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();
    let stdout_task = tokio::spawn(stream_playwright_stdout(
        stdout_handle,
        proj_pool.clone(),
        channels.clone(),
        job_id,
    ));
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut err) = stderr_handle {
            let _ = err.read_to_end(&mut buf).await;
        }
        buf
    });

    let exit_code = esito_del_processo(child, proj_pool, job_id, timeout_s).await?;

    let (stdout_bytes, live_progress) = stdout_task.await.unwrap_or_else(|_| {
        (
            Vec::new(),
            crate::playwright_live::PlaywrightProgress::default(),
        )
    });
    let stderr_bytes = stderr_task.await.unwrap_or_default();
    Ok(OutputProcesso {
        exit_code,
        stdout: String::from_utf8_lossy(&stdout_bytes).to_string(),
        stderr: String::from_utf8_lossy(&stderr_bytes).to_string(),
        live_progress,
    })
}

/// Attende la fine del processo entro il tetto. Un'attesa che non si conclude
/// CHIUDE la riga `jobs` prima di propagare l'errore: prima restava `running`
/// per sempre, e il pannello mostrava come suite in corso un processo ucciso.
async fn esito_del_processo(
    child: &mut tokio::process::Child,
    proj_pool: &sqlx::PgPool,
    job_id: Uuid,
    timeout_s: u64,
) -> Result<Option<i32>, String> {
    match tokio::time::timeout(Duration::from_secs(timeout_s), child.wait()).await {
        Ok(Ok(status)) => Ok(Some(status.code().unwrap_or(-1))),
        Ok(Err(e)) => {
            let motivo = format!("errore attesa processo: {e}");
            chiudi_job_non_concluso(proj_pool, job_id, &motivo).await;
            Err(motivo)
        }
        Err(_) => {
            let _ = child.start_kill();
            let motivo = format!("timeout dopo {timeout_s}s: i test sono stati interrotti");
            chiudi_job_non_concluso(proj_pool, job_id, &motivo).await;
            Err(format!(
                "Timeout dopo {timeout_s}s. I test sono stati interrotti. Considera di \
                 aumentare il timeout o di filtrare i test."
            ))
        }
    }
}

/// Chiude una riga `jobs` rimasta senza esito (timeout, errore di attesa).
async fn chiudi_job_non_concluso(proj_pool: &sqlx::PgPool, job_id: Uuid, motivo: &str) {
    let _ = sqlx::query(
        "UPDATE jobs SET status = 'failed', \
         input = jsonb_set( \
           jsonb_set(input, '{label}', to_jsonb($1::text), true), \
           '{message}', to_jsonb($2::text), true) \
         WHERE id = $3",
    )
    .bind("Interrotto")
    .bind(motivo)
    .bind(job_id)
    .execute(proj_pool)
    .await;
}

/// Esegue attraverso l'esecutore unico una suite che un tool generico
/// (`run_command`, `run_tests`) ha ricevuto come riga di comando.
///
/// Chiamata solo da `playwright_cli::intercetta_suite`, che ha gia' stabilito
/// che la riga chiede la suite e che non porta altri comandi. Qui si traduce
/// la riga nei parametri del runner: gli argomenti passano interi
/// (`extra_args`), la BASE_URL dichiarata inline diventa il parametro
/// omonimo, la directory viene da `cd`/`working_dir`. Cio' che non e'
/// traducibile viene DICHIARATO nella nota, mai applicato a meta': un'env var
/// silenziosamente caduta e' un test che fallisce per una ragione che non si
/// legge da nessuna parte.
///
/// Il `timeout_secs` del tool generico non viene propagato: e' il tempo che quel
/// tool aspetta, e vale al massimo 300s contro i 600 del runner. Propagarlo
/// accorcerebbe la finestra di una suite E2E fino a troncarla a meta', che e'
/// il modo di ottenere un job appeso invece di un esito.
pub(super) async fn esegui_suite_delegata(
    ctx: &AgentToolContext,
    tool: &str,
    inv: &super::playwright_cli::InvocazioneSuite,
    working_dir_param: Option<&str>,
    riga: &str,
) -> RispostaTool {
    let input = input_per_runner(inv, working_dir_param, riga);
    let coda = avvertenze_di_traduzione(inv);
    let out = tool_run_playwright_tests(ctx, &input).await;
    // La premessa si concatena al solo TESTO: esito, natura ed exit code
    // restano nei campi e nessuna prosa anteposta li puo' coprire.
    //
    // Era il caso che motivava `prepend_preserving_failure`, e vale la pena
    // ricordare cosa succedeva senza: il fallimento viveva col marker IN TESTA
    // alla stringa, un `format!` davanti lo spingeva in mezzo, e
    // `is_tool_failure` — che guarda solo la testa — non lo vedeva piu'. Una
    // suite ROSSA passava per verde a tutti i consumatori a valle. Il ponte
    // rimediava rimettendo il marker in testa alla composizione; col campo non
    // c'e' piu' niente da rimettere a posto, ed e' l'ultimo suo chiamante di
    // produzione a sparire.
    RispostaTool {
        testo: format!(
            "[{tool} -> run_playwright_tests] La suite Playwright ha un solo esecutore: BASE_URL dalle \
             porte allocate al progetto, preflight Chromium, attesa del servizio bersaglio e \
             registrazione nel pannello Playwright. La riga e' stata eseguita da li' con i suoi \
             argomenti.{coda}\n{}",
            out.testo
        ),
        ..out
    }
}

/// Le due variabili con cui una riga dichiara l'URL del bersaglio: sono le
/// stesse che `spawn_playwright_child` inietta, quindi tradurle nel parametro
/// `base_url` le fa arrivare esattamente dove sarebbero arrivate.
const ENV_BASE_URL: [&str; 2] = ["BASE_URL", "PLAYWRIGHT_BASE_URL"];

/// Traduce la riga nei parametri del runner (vedi [`esegui_suite_delegata`]).
fn input_per_runner(
    inv: &super::playwright_cli::InvocazioneSuite,
    working_dir_param: Option<&str>,
    riga: &str,
) -> Value {
    let mut input = serde_json::json!({
        "extra_args": inv.args,
        // Il cleanup delle config wrapper CANCELLA file dal progetto: e'
        // un'azione che chi ha scritto `npx playwright test` non ha chiesto.
        // Resta disponibile a chi invoca il tool dedicato di proposito.
        "cleanup_stale_configs": false,
    });
    if let Some(dir) = directory_della_suite(inv, working_dir_param, riga) {
        input["config_path"] = serde_json::json!(dir);
    }
    if let Some((_, url)) = inv
        .env_inline
        .iter()
        .find(|(n, _)| ENV_BASE_URL.contains(&n.as_str()))
    {
        input["base_url"] = serde_json::json!(url);
    }
    input
}

/// Cio' che la riga chiedeva e la traduzione non porta con se', dichiarato in
/// coda all'output. Vuoto se non c'e' niente da dichiarare.
fn avvertenze_di_traduzione(inv: &super::playwright_cli::InvocazioneSuite) -> String {
    let mut avvertenze: Vec<String> = Vec::new();
    let env_cadute: Vec<&str> = inv
        .env_inline
        .iter()
        .filter(|(n, _)| !ENV_BASE_URL.contains(&n.as_str()))
        .map(|(n, _)| n.as_str())
        .collect();
    if !env_cadute.is_empty() {
        avvertenze.push(format!(
            "variabili inline NON applicate ({}): il runner isola l'ambiente del processo. \
             Se servono ai test, mettile nella config Playwright.",
            env_cadute.join(", ")
        ));
    }
    if inv.redirezioni {
        avvertenze.push(
            "redirezioni di output ignorate: l'output della suite torna qui per intero.".to_string(),
        );
    }
    if avvertenze.is_empty() {
        String::new()
    } else {
        format!("\nAvvertenze: {}", avvertenze.join(" | "))
    }
}

/// Directory in cui cercare la config Playwright, relativa alla root del
/// progetto: `working_dir` del tool e `cd` della riga si sommano, salvo quando
/// il secondo ripete il primo — caso che riconosce gia'
/// `helpers::detect_workdir_path_duplication` (punto unico, regola L: e' la
/// stessa domanda che `run_command` pone prima di eseguire).
fn directory_della_suite(
    inv: &super::playwright_cli::InvocazioneSuite,
    working_dir_param: Option<&str>,
    riga: &str,
) -> Option<String> {
    let wd = working_dir_param.filter(|s| !s.trim().is_empty());
    match (wd, inv.cd.as_deref()) {
        (None, None) => None,
        (Some(w), None) => Some(w.to_string()),
        (None, Some(c)) => Some(c.to_string()),
        (Some(w), Some(c)) => {
            if super::helpers::detect_workdir_path_duplication(w, riga).is_some() {
                Some(w.to_string())
            } else {
                Some(format!(
                    "{}/{}",
                    w.trim_end_matches('/'),
                    c.trim_start_matches("./")
                ))
            }
        }
    }
}

pub(super) async fn tool_run_playwright_tests(
    ctx: &AgentToolContext,
    input: &Value,
) -> RispostaTool {
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
        extra_args,
    } = params;

    // ── 2. Controllo presenza Playwright ─────────────────────────────────────
    let (playwright_root, stale_configs) = match resolve_playwright_root(ctx, &config_path_override)
    {
        Ok(v) => v,
        Err(risposta) => return risposta,
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
        // RIMEDIABILE per costruzione: il messaggio non dice solo che manca,
        // detta le due righe che lo installano.
        return RispostaTool::fallito_rimediabile(format!(
            "[run_playwright_tests] Playwright non trovato nel progetto (cercato in {} e sottodirectory).\n\
             Installa con: run_command({{\"command\": \"pnpm add -D @playwright/test\", \"working_dir\": \"app\"}}).\n\
             Poi inizializza: run_command({{\"command\": \
             \"npx playwright install --with-deps chromium\", \"working_dir\": \"app\"}}).",
            ctx.root_path.display()
        ));
    }

    // ── 2bis. Pre-flight check librerie sistema chromium-headless-shell ──────
    if let Err(risposta) = run_preflight_check(ctx, root).await {
        return risposta;
    }

    // ── 3. Server raggiungibile? (pre-volo del tool: l'avvio automatico e'
    //      una facolta' dell'agente, non della verifica) ─────────────────────
    let port_rows: Vec<(i32, String)> = sqlx::query_as(
        "SELECT port, label FROM nexus_port_allocations WHERE project_id = $1 ORDER BY port ASC",
    )
    .bind(ctx.project_id)
    .fetch_all(&*ctx.db)
    .await
    .unwrap_or_default();
    let base_url_previsto = determine_base_url(explicit_base_url, &port_rows).await;
    let server_status =
        check_server_status(ctx, root, base_url_previsto.as_deref(), auto_start).await;

    // ── 4. Verifica a suite (punto unico): memoria + esecuzione + flaky ──────
    let command_str = build_playwright_command(
        test_timeout_ms,
        workers,
        &reporter,
        &project_arg,
        &filter,
        &extra_args,
    );

    // La directory si esprime RELATIVA alla radice del run, come fa il criterio
    // del gate col `working_dir` dello step: e' l'altra meta' del
    // riconoscimento reciproco (la prima e' la normalizzazione del comando in
    // `suite_key`). Con la playwright root passata come radice, la stessa suite
    // avrebbe avuto chiave "" per l'agente e "app" per il gate, e i due non si
    // sarebbero riconosciuti mai.
    let dir_relativa = playwright_root
        .strip_prefix(&ctx.root_path)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .filter(|s| !s.is_empty());
    let deps = suite_deps(
        SuiteEnv::dal_ctx(ctx, ctx.root_path.clone()),
        ctx.root_path.clone(),
    )
    .await;

    let inv =
        crate::suite_verification::SuiteInvocation::suite(command_str, dir_relativa, timeout);
    let verifica = match deps.verifica(&inv).await {
        Ok(v) => v,
        // Fallimento dell'INFRASTRUTTURA di verifica (memoria suite, chiave di
        // stato), non della suite: e' comunque un tool fallito, e lo dichiara
        // dal ponte come i dodici altri rami di questo file — nudo, il modello
        // lo leggeva come un successo senza esito.
        // DEL SISTEMA: e' la memoria della suite o la chiave di stato a non
        // rispondere, non la suite. Nessun parametro della chiamata la rimette
        // in piedi, e ripetere identico rifallisce.
        Err(e) => return RispostaTool::fallito_di_sistema(format!("[run_playwright_tests] {e}")),
    };

    // ── 5. Output finale: note del pre-volo + esito dichiarato dal punto unico
    let mut out = String::new();
    if !cleanup_notes.is_empty() {
        out.push_str(&format!("Cleanup:\n  {}\n", cleanup_notes.join("\n  ")));
    }
    out.push_str(&format!("Server: {server_status}\n"));
    out.push_str(&verifica.testo);
    risposta_da_verifica(&verifica, out)
}

/// Traduce l'esito GIA' strutturato della verifica nei campi di [`RispostaTool`].
///
/// `SuiteVerification` porta `outcome` (vocabolario canonico `passed|flaky|
/// tests_failed|setup_failed`, punto unico `suite_verification`) ed `exit_code`,
/// e questo tool li appiattiva entrambi nel testo: chi a valle voleva sapere
/// com'era andata la suite doveva rileggere la prosa, cioe' esattamente cio' che
/// la regola M vieta e che la regola Q rende inutile.
///
/// I quattro casi non collassano in due:
/// - `Passed` -> riuscito;
/// - `Flaky` -> RIUSCITO, per il contratto gia' deciso in `suite_verification`
///   (un fallito i cui test ripassano alla riesecuzione mirata non apre il ciclo
///   di correzione e non boccia il gate; resta debito di TEST, e il testo lo
///   dice);
/// - `TestsFailed` -> tool RIUSCITO con `exit_code` valorizzato. Il tool ha
///   fatto il suo lavoro — ha eseguito e ha riportato — e i test rossi sono
///   l'esito del COMANDO. E' la stessa distinzione su cui il final_gate decide
///   se rieseguire il criterio o correggere il codice;
/// - `SetupFailed` -> tool FALLITO: la suite non e' mai partita, quindi non c'e'
///   nessun esito di test da riportare. TRANSITORIO perche' la causa tipica e'
///   l'ambiente non ancora pronto (servizio bersaglio freddo, Chromium in
///   installazione), dove ritentare e' la strategia corretta.
fn risposta_da_verifica(
    verifica: &crate::suite_verification::SuiteVerification,
    testo: String,
) -> RispostaTool {
    match verifica.outcome {
        SuiteOutcome::SetupFailed => RispostaTool::fallito_transitorio(testo),
        // `exit_code` None = timeout o processo mai terminato: si dichiara
        // comunque un comando andato male, senza inventare un numero preciso.
        SuiteOutcome::TestsFailed => {
            RispostaTool::comando(testo, verifica.exit_code.unwrap_or(-1))
        }
        SuiteOutcome::Passed | SuiteOutcome::Flaky => RispostaTool::riuscito(testo),
    }
}

// L'esito strutturato (regola M) vive nel punto unico
// [`crate::suite_verification::SuiteOutcome`]: la stessa domanda se la ponevano
// il tool, il gate e il ciclo review, e finche' il vocabolario e' stato qui la
// risposta di questo modulo non era visibile agli altri due — in particolare
// non lo era `flaky`, che nasce dalla riesecuzione mirata e non da una singola
// esecuzione.

/// Estrae una causa breve per un fallimento di setup dall'ultima riga non
/// vuota del segnale disponibile (stderr ha priorita', poi stdout): non e'
/// parsing della prosa per DECIDERE l'esito (quello lo fa esclusivamente
/// classify_playwright_outcome sui segnali strutturati), e' l'unico testo che
/// il runner ha effettivamente prodotto sul perche', preso senza
/// pattern-matching sul contenuto. Troncata a 300 char.
fn extract_failure_cause(stdout: &str, stderr: &str) -> Option<String> {
    let last_line_of = |text: &str| -> Option<String> {
        text.lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(|l| l.chars().take(300).collect())
    };
    last_line_of(stderr).or_else(|| last_line_of(stdout))
}

/// Esito del run pronto per la persistenza: `status` e' il valore della colonna
/// `jobs.status`, `outcome` l'identificatore canonico (regola N) e
/// `failure_cause` e' valorizzato solo per un setup fallito.
///
/// Qui l'esito e' quello di UNA esecuzione: `flaky` non puo' comparire, perche'
/// nasce dal confronto con la riesecuzione mirata e lo scrive il punto unico
/// ([`crate::suite_verification::memo`]) quando la classificazione e' fatta.
struct PlaywrightResultSummary {
    status: &'static str,
    outcome: &'static str,
    label: String,
    msg: String,
    failure_cause: Option<String>,
}

/// Costruisce il descrittore dell'esito (label + message) da usare nel record
/// `jobs` e negli eventi. Distingue "setup fallito, zero test eseguiti" da "N
/// passati, M falliti": prima entrambi i casi producevano lo stesso "0
/// passati, 0 falliti" fuorviante quando il webServer non partiva mai (regola
/// M: lo stato viene dal segnale strutturato, non da un testo che finge un
/// risultato di test che non e' mai iniziato).
fn playwright_result_summary(
    exit_code: Option<i32>,
    stats: &SuiteStats,
    stdout: &str,
    stderr: &str,
) -> PlaywrightResultSummary {
    let outcome = crate::suite_verification::classifica_esito(exit_code, stats.eseguiti());
    let status = outcome.job_status();

    if outcome == SuiteOutcome::SetupFailed {
        setup_failed_summary(status, outcome, stdout, stderr)
    } else {
        tests_result_summary(status, outcome, stats)
    }
}

/// Descrittore per un run che non ha eseguito nemmeno un test: la causa viene
/// dall'ultima riga del runner (extract_failure_cause), mai da un conteggio
/// di test che non e' mai iniziato.
fn setup_failed_summary(
    status: &'static str,
    outcome: SuiteOutcome,
    stdout: &str,
    stderr: &str,
) -> PlaywrightResultSummary {
    let failure_cause = extract_failure_cause(stdout, stderr);
    let label = "Setup fallito (nessun test eseguito)".to_string();
    let msg = match &failure_cause {
        Some(cause) => format!("Setup fallito, nessun test eseguito. Causa: {cause}"),
        None => "Setup fallito, nessun test eseguito.".to_string(),
    };
    PlaywrightResultSummary {
        status,
        outcome: outcome.as_str(),
        label,
        msg,
        failure_cause,
    }
}

/// Descrittore per un run che ha eseguito almeno un test (Passed o TestsFailed).
fn tests_result_summary(
    status: &'static str,
    outcome: SuiteOutcome,
    stats: &SuiteStats,
) -> PlaywrightResultSummary {
    let label = if outcome == SuiteOutcome::Passed {
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
    PlaywrightResultSummary {
        status,
        outcome: outcome.as_str(),
        label,
        msg,
        failure_cause: None,
    }
}

/// Calcola il progress finale: privilegia le stats del parser completo, ma
/// preserva flaky/failed_specs accumulati live se disponibili.
fn build_final_progress(
    stats: &SuiteStats,
    live_progress: &crate::playwright_live::PlaywrightProgress,
) -> crate::playwright_live::PlaywrightProgress {
    crate::playwright_live::PlaywrightProgress {
        total: live_progress.total.or(Some(stats.eseguiti() as u32)),
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
    env: &SuiteEnv,
    proj_pool: &sqlx::PgPool,
    job_id: Uuid,
    exit_code: Option<i32>,
    stats: &SuiteStats,
    live_progress: &crate::playwright_live::PlaywrightProgress,
    artifacts: &[serde_json::Value],
    command_str: &str,
    stdout: &str,
    stderr: &str,
) {
    let summary = playwright_result_summary(exit_code, stats, stdout, stderr);
    let final_progress = build_final_progress(stats, live_progress);

    update_playwright_job_record(
        proj_pool,
        env.project_id,
        job_id,
        &summary,
        artifacts,
        command_str,
        exit_code,
        &final_progress,
    )
    .await;

    emit_playwright_final_events(env, job_id, &summary, artifacts, exit_code, final_progress);
}

/// UPDATE del record `jobs` con esito finale (status, input, progress).
/// Separazione DB per-progetto: usa il pool del progetto gia' risolto.
#[allow(clippy::too_many_arguments)]
async fn update_playwright_job_record(
    proj_pool: &sqlx::PgPool,
    pid: Uuid,
    job_id: Uuid,
    summary: &PlaywrightResultSummary,
    artifacts: &[serde_json::Value],
    command_str: &str,
    exit_code: Option<i32>,
    final_progress: &crate::playwright_live::PlaywrightProgress,
) {
    match sqlx::query("UPDATE jobs SET status = $1, input = $2, progress = $3 WHERE id = $4")
        .bind(summary.status)
        .bind(serde_json::json!({
            "label": summary.label,
            "message": summary.msg,
            "artifacts": artifacts,
            "command": command_str,
            "exit_code": exit_code,
            "outcome": summary.outcome,
            "failure_cause": summary.failure_cause,
        }))
        .bind(serde_json::to_value(final_progress).unwrap_or(serde_json::json!({})))
        .bind(job_id)
        .execute(proj_pool)
        .await
    {
        Ok(r) => {
            tracing::info!(
                rows = r.rows_affected(),
                project_id = %pid,
                status = %summary.status,
                outcome = %summary.outcome,
                artifacts = artifacts.len(),
                "playwright_test job aggiornato"
            )
        }
        Err(e) => {
            tracing::error!(error = %e, project_id = %pid, "playwright_test job UPDATE fallito")
        }
    }
}

/// Emette gli eventi di esito (dispatcher JobCreated + PlaywrightEvent::Final)
/// e programma il cleanup deferito del channel SSE (30s).
fn emit_playwright_final_events(
    env: &SuiteEnv,
    job_id: Uuid,
    summary: &PlaywrightResultSummary,
    artifacts: &[serde_json::Value],
    exit_code: Option<i32>,
    final_progress: crate::playwright_live::PlaywrightProgress,
) {
    // Dispatcher: notifica esito finale → toast + highlight pannello Playwright
    if let Some(pc) = &env.project_channels {
        nexus_events::dispatcher::emit(
            pc,
            env.project_id,
            nexus_events::event::ProjectEvent::JobCreated {
                id: job_id,
                job_kind: "playwright_test".to_string(),
                status: summary.status.to_string(),
                label: summary.label.clone(),
                summary: Some(summary.msg.clone()),
                artifacts: serde_json::to_value(artifacts).unwrap_or(serde_json::Value::Null),
            },
        );
    }

    let Some(channels) = env.playwright_channels.clone() else {
        return;
    };
    // Emette evento terminale agli SSE consumer + rimuove channel
    crate::playwright_live::emit(
        &channels,
        crate::playwright_live::PlaywrightEvent::Final {
            job_id,
            status: summary.status.to_string(),
            exit_code: exit_code.unwrap_or(-1),
            progress: final_progress,
        },
    );
    // Lascia il channel attivo per qualche secondo: i consumer SSE che si
    // collegano DOPO il termine devono comunque ricevere il Final.
    // Cleanup deferito.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        crate::playwright_live::unregister(&channels, job_id);
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

/// Formatta il report testuale del run (tool_result e evidence del criterio).
fn format_playwright_run_output(
    root: &Path,
    amb: &AmbienteSuite,
    command_str: &str,
    exit_code: Option<i32>,
    stats: &SuiteStats,
    out: &OutputProcesso,
) -> String {
    let (port_rows, base_url, backend_api_url) = (
        amb.port_rows.as_slice(),
        amb.base_url.as_deref(),
        amb.backend_api_url.as_deref(),
    );
    let stdout_tail = last_n_lines(&out.stdout, 60);
    let stderr_excerpt = first_n_nonempty_lines(&out.stderr, 20);

    let status_label = if exit_code == Some(0) {
        "TUTTI I TEST PASSATI"
    } else {
        "TEST FALLITI"
    };
    let port_info = format_port_info(port_rows);

    format!(
        "=== PLAYWRIGHT TEST ===\n\
         Stato: {status_label} (exit code: {exit_code})\n\
         Playwright root: {pw_root}\n\
         Porte progetto: {port_info}\n\
         BASE_URL: {base_url_display}\n\
         BACKEND_API_URL: {backend_api_url_display}\n\
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
        status_label = status_label,
        exit_code = exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "n/d".to_string()),
        port_info = if port_info.is_empty() {
            "nessuna porta allocata".to_string()
        } else {
            port_info
        },
        base_url_display = base_url.unwrap_or("(da playwright.config.ts)"),
        backend_api_url_display = backend_api_url
            .unwrap_or("(non trovata — verifica label 'backend-*' in nexus_port_allocations)"),
        command_str = command_str,
        passed = stats.passed,
        failed = stats.failed,
        skipped = stats.skipped,
        total = stats.eseguiti(),
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
fn parse_playwright_output_stats(stdout: &str, stderr: &str) -> SuiteStats {
    let combined = format!("{stdout}\n{stderr}");
    let mut stats = SuiteStats::default();

    for line in combined.lines() {
        let t = line.trim();

        // "  3 passed (5s)"  oppure "  2 passed, 1 failed (12s)"
        for kw in &["passed", "failed", "skipped", "flaky"] {
            if let Some(n) = extract_stat(t, kw) {
                match *kw {
                    "passed" => stats.passed += n,
                    "failed" => stats.failed += n,
                    "skipped" => stats.skipped += n,
                    "flaky" => stats.flaky_reported += n,
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

// I conteggi vivono nel punto unico: `SuiteStats`. La copia locale rendeva
// impossibile passare l'esito di UNA esecuzione a chi doveva confrontarne DUE.

// ── Tool Fase 3: test singolo, lint fix, format file ──────────────────

/// Risolve la working directory relativa alla root del progetto. Ritorna
/// direttamente la stringa d'errore gia' formattata (pronta per il return del
/// tool) se il path e' fuori dalla root o invalido. Punto unico condiviso dai
/// tool testing/lint (regola L).
fn resolve_work_path(
    ctx: &AgentToolContext,
    working_dir: &str,
) -> Result<std::path::PathBuf, RispostaTool> {
    if working_dir.is_empty() {
        return Ok(ctx.root_path.clone());
    }
    match resolve_relative_path(&ctx.root_path, working_dir) {
        Ok(p) => Ok(p),
        // Il percorso e' un parametro che l'agente controlla, e il resolver dice
        // gia' fin dove esiste: rimediabile con l'informazione a bordo.
        Err(e) => Err(RispostaTool::fallito_rimediabile(format!(
            "[Errore percorso: {}]",
            e.1["error"].as_str().unwrap_or("path error")
        ))),
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
pub(super) async fn tool_run_specific_test(
    ctx: &AgentToolContext,
    input: &Value,
) -> RispostaTool {
    use nexus_agent_tools::{input_contract::InputTool, tool_inputs::RunSpecificTestInput};

    let params = match RunSpecificTestInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let timeout_secs = params.timeout_secs.unwrap_or(120).clamp(0, 600) as u64;

    let work_path = match resolve_work_path(ctx, params.working_dir.as_deref().unwrap_or("")) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };

    let command = match detect_test_command(&work_path, &params.test_name) {
        Some(c) => c,
        // RIMEDIABILE: l'agente puo' indicare un'altra `working_dir`, e il
        // messaggio elenca i marker cercati — cioe' cosa deve esserci perche'
        // il rilevamento riesca.
        None => {
            return RispostaTool::fallito_rimediabile(format!(
                "[Errore: framework di test non rilevato in '{}'. \
                 File cercati: Cargo.toml, package.json, pytest.ini, pyproject.toml, mix.exs, go.mod]",
                work_path.display()
            ))
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
pub(super) async fn tool_run_lint_fix(ctx: &AgentToolContext, input: &Value) -> RispostaTool {
    use nexus_agent_tools::{input_contract::InputTool, tool_inputs::RunLintFixInput};

    if let Some(negato) = permesso_di_scrittura(ctx) {
        return negato;
    }
    let params = match RunLintFixInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let timeout_secs = params.timeout_secs.unwrap_or(120).clamp(0, 300) as u64;

    let work_path = match resolve_work_path(ctx, params.working_dir.as_deref().unwrap_or("")) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };

    let command = match detect_lint_command(&work_path, params.check_only.unwrap_or(false)) {
        Some(c) => c,
        None => {
            return RispostaTool::fallito_rimediabile(format!(
                "[Errore: linter non rilevato in '{}'. \
                 Supportati: cargo clippy (Rust), eslint (Node), ruff (Python)]",
                work_path.display()
            ))
        }
    };

    run_test_command(ctx, &command, &work_path, timeout_secs).await
}

/// Il rifiuto per permesso di scrittura mancante, in un punto solo.
///
/// DEL SISTEMA, sempre: il permesso e' una decisione del progetto sul run, non
/// un parametro della chiamata. Riformularla non la cambia, e ripeterla e' il
/// solo modo di sprecare iterazioni contro una porta chiusa. Stesso precedente
/// del permesso di scrittura negato in `write_file`.
fn permesso_di_scrittura(ctx: &AgentToolContext) -> Option<RispostaTool> {
    if ctx.can_write {
        return None;
    }
    Some(RispostaTool::fallito_di_sistema(
        "[Errore: permesso di scrittura non concesso]",
    ))
}

/// Formatta un singolo file (rustfmt, prettier, black) in base all'estensione.
pub(super) async fn tool_format_file(ctx: &AgentToolContext, input: &Value) -> RispostaTool {
    use nexus_agent_tools::{input_contract::InputTool, tool_inputs::FormatFileInput};

    if let Some(negato) = permesso_di_scrittura(ctx) {
        return negato;
    }
    let params = match FormatFileInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let path_str = params.path.as_str();

    let target = match resolve_relative_path(&ctx.root_path, path_str) {
        Ok(p) => p,
        Err(e) => {
            return RispostaTool::fallito_rimediabile(format!(
                "[Errore percorso: {}]",
                e.1["error"].as_str().unwrap_or("path error")
            ))
        }
    };
    if !target.is_file() {
        return RispostaTool::fallito_rimediabile(format!("[Errore: '{path_str}' non e' un file]"));
    }

    let ext = target
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let command = match detect_format_command(&ext, &target, params.check_only.unwrap_or(false)) {
        Some(c) => c,
        None => {
            return RispostaTool::fallito_rimediabile(format!(
                "[Errore: formatter non disponibile per estensione '.{ext}'. \
                 Supportati: .rs (rustfmt), .ts/.js/.json/.css/.md (prettier), .py (black), .go (gofmt)]"
            ))
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
/// DIVERGENZA CHIUSA: lo stato d'uscita finiva nel testo come `Exit code: N`, e
/// il ponte legacy cerca `EXIT CODE: ` MAIUSCOLO. Le due scritture non si sono
/// mai incontrate: per i tre tool che passano di qui — `run_specific_test`,
/// `run_lint_fix`, `format_file` — `RispostaTool.exit_code` e' stato `None`
/// SEMPRE, e con esso il dato su cui il final_gate decide se rieseguire un
/// criterio o correggere il codice. Nessun test poteva vederlo: il testo era
/// corretto e leggibile, mancava solo chi lo rileggesse con la stessa
/// convenzione di chi lo scriveva.
///
/// Ora il campo e' il campo. Il testo continua a dirlo perche' lo legge il
/// modello, ma nessun codice lo analizza piu' per saperlo (regola Q, punto 3).
///
/// L'esito resta `Riuscito` anche a `exit_code != 0`: il tool ha fatto il suo
/// lavoro — ha eseguito e ha riportato — e un lint rosso e' un COMANDO fallito,
/// non un tool fallito. Collassarli renderebbe un test rotto indistinguibile da
/// un runner che non e' partito.
async fn run_test_command(
    _ctx: &AgentToolContext,
    command: &str,
    work_dir: &Path,
    timeout_secs: u64,
) -> RispostaTool {
    let (exit_code, stdout, stderr) =
        match spawn_and_capture_output(command, work_dir, timeout_secs).await {
            Ok(v) => v,
            Err(risposta) => return risposta,
        };

    // Tronca output se troppo lungo (coda degli ultimi 6000 byte)
    let stdout_tail = truncate_output_tail(stdout, 6000);
    let stderr_tail = truncate_output_tail(stderr, 6000);

    let mut result = format!("Exit code: {exit_code}\n");
    if !stdout_tail.is_empty() {
        result.push_str(&format!("\nOutput:\n{stdout_tail}"));
    }
    if !stderr_tail.is_empty() {
        result.push_str(&format!("\nErrori:\n{stderr_tail}"));
    }
    RispostaTool::comando(result, exit_code)
}

/// Lancia `command` nella shell isolata, cattura stdout/stderr in parallelo a
/// `child.wait()` (evita deadlock del buffer pipe ~64 KB) con timeout. Ritorna
/// (exit_code, stdout, stderr) oppure Err con il messaggio d'errore pronto.
async fn spawn_and_capture_output(
    command: &str,
    work_dir: &Path,
    timeout_secs: u64,
) -> Result<(i32, String, String), RispostaTool> {
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
        // L'avvio del processo dipende dalla shell isolata e dall'ambiente, non
        // da come l'agente ha scritto la riga: quella non e' ancora stata letta
        // da nessuno.
        Err(e) => return Err(RispostaTool::fallito_di_sistema(format!("[Errore avvio: {e}]"))),
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
        Ok(Err(e)) => {
            return Err(RispostaTool::fallito_di_sistema(format!(
                "[Errore attesa processo: {e}]"
            )))
        }
        Err(_) => {
            let _ = child.start_kill();
            // Come nel ramo timeout di `command.rs`: droppare un `JoinHandle`
            // STACCA il task invece di annullarlo, e i due drainer restano in
            // `read_to_end` su una pipe che i nipoti del processo ucciso
            // tengono aperta. Qui il `return` anticipato li lasciava vivi per
            // sempre.
            stdout_task.abort();
            stderr_task.abort();
            // Prima usciva NUDO, senza marker: un comando ucciso dal timeout
            // arrivava all'agente come un'esecuzione riuscita di cui mancava
            // solo l'output. TRANSITORIO come il servizio che non entra in
            // ascolto entro la sua finestra, e il testo nomina il parametro che
            // da' l'altra strada.
            return Err(RispostaTool::fallito_transitorio(format!(
                "[Timeout dopo {timeout_secs}s. Comando: {command}.                  Se il comando e' legittimamente lento alza 'timeout_secs'.]"
            )));
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

#[cfg(test)]
mod comando_delegato_tests {
    use super::*;

    /// LA PROVA del difetto chiuso: la premessa non deve nascondere l'esito.
    ///
    /// `esegui_suite_delegata` anteponeva la propria spiegazione con un
    /// `format!`, e `tool_run_playwright_tests` dichiara il fallimento col
    /// marker IN TESTA: il testo davanti lo spingeva in mezzo alla stringa, dove
    /// `is_tool_failure` — che guarda solo la testa — non lo vedeva piu'. Una
    /// suite rossa arrivava ai consumatori a valle come riuscita.
    ///
    /// Il test misura la COMPOSIZIONE, cioe' quello che il chiamante fa davvero,
    /// partendo dal produttore reale del fallimento (`tool_failure`) e non da
    /// una stringa scritta a mano col marker copiato (regola O).
    ///
    /// MUTAZIONE: tornare al `format!` che antepone la premessa con una
    /// concatenazione fa rosseggiare la prima asserzione — il valore del
    /// difetto reale.
    #[test]
    fn la_premessa_non_nasconde_il_fallimento_della_suite() {
        use nexus_types::tool_outcome::{is_tool_failure, prepend_preserving_failure, tool_failure};

        let esito_rosso = tool_failure("3 test falliti su 41");
        assert!(is_tool_failure(&esito_rosso), "premessa del test: il produttore dichiara il fallimento");

        let composto = prepend_preserving_failure(
            "[run_command -> run_playwright_tests] La suite Playwright ha un solo esecutore.",
            &esito_rosso,
        );
        assert!(
            is_tool_failure(&composto),
            "dopo la premessa il fallimento deve restare riconoscibile: {composto}"
        );
        // La premessa c'e' comunque, e il corpo non ha un secondo marker in mezzo.
        assert!(composto.contains("un solo esecutore"));
        assert!(composto.contains("3 test falliti su 41"));

        // E un esito RIUSCITO non diventa un fallimento per il solo fatto di
        // avere una premessa davanti.
        let composto_ok = prepend_preserving_failure(
            "[run_command -> run_playwright_tests] La suite Playwright ha un solo esecutore.",
            "41 test passati",
        );
        assert!(!is_tool_failure(&composto_ok));
    }

    /// Gli argomenti NON si costruiscono a mano: si prendono da dove nascono in
    /// produzione, cioe' dalla riga scritta dall'agente passata per
    /// `invocazione_suite` (regola O).
    fn args_dalla_riga(riga: &str) -> Vec<String> {
        super::super::playwright_cli::invocazione_suite(riga)
            .expect("la riga deve essere riconosciuta come suite")
            .args
    }

    #[test]
    fn gli_argomenti_dell_agente_arrivano_nella_riga_eseguita() {
        let args = args_dalla_riga("npx playwright test e2e/auth.spec.ts --headed");
        let cmd = build_playwright_command(10_000, 1, "list", &None, &None, &args);
        assert!(cmd.contains("e2e/auth.spec.ts"), "riga costruita: {cmd}");
        assert!(cmd.contains("--headed"), "riga costruita: {cmd}");
    }

    /// Un default omologo a quello che l'agente ha scritto non va aggiunto: due
    /// `--workers` sulla stessa riga rendono il valore vincente un fatto
    /// dell'ordine, che nessun chiamante controlla.
    #[test]
    fn il_default_non_si_somma_al_flag_dell_agente() {
        let args = args_dalla_riga("npx playwright test --workers 4");
        let cmd = build_playwright_command(10_000, 1, "list", &None, &None, &args);
        assert_eq!(
            cmd.matches("--workers").count(),
            1,
            "un solo --workers, riga costruita: {cmd}"
        );
        assert!(cmd.contains("--workers 4"), "riga costruita: {cmd}");
        // La forma attaccata deve valere quanto quella staccata.
        let args_eq = args_dalla_riga("npx playwright test --reporter=html");
        let cmd_eq = build_playwright_command(10_000, 1, "list", &None, &None, &args_eq);
        assert_eq!(
            cmd_eq.matches("--reporter").count(),
            1,
            "un solo --reporter, riga costruita: {cmd_eq}"
        );
    }

    /// La riga costruita torna a essere UNA stringa per `sh -c`: un argomento
    /// con spazi, che l'agente aveva quotato, si spezzerebbe in due.
    #[test]
    fn un_argomento_con_spazi_resta_un_argomento() {
        let args = args_dalla_riga("npx playwright test --grep \"login utente\"");
        assert_eq!(args, vec!["--grep".to_string(), "login utente".to_string()]);
        let cmd = build_playwright_command(10_000, 1, "list", &None, &None, &args);
        assert!(
            cmd.contains("'login utente'"),
            "l'argomento va ri-quotato, riga costruita: {cmd}"
        );
    }

    /// `cd` e `working_dir` insieme non si sommano quando il secondo ripete il
    /// primo: e' la stessa domanda che `run_command` pone prima di eseguire, e
    /// si delega al suo punto unico.
    #[test]
    fn directory_non_raddoppia_quando_il_cd_ripete_il_working_dir() {
        let inv = super::super::playwright_cli::invocazione_suite("cd app && npx playwright test")
            .expect("suite riconosciuta");
        assert_eq!(
            directory_della_suite(&inv, Some("app"), "cd app && npx playwright test").as_deref(),
            Some("app")
        );
        assert_eq!(
            directory_della_suite(&inv, Some("packages"), "cd app && npx playwright test")
                .as_deref(),
            Some("packages/app"),
            "senza ripetizione le due parti si sommano"
        );
    }
}

#[cfg(test)]
mod playwright_outcome_tests {
    use super::*;

    #[test]
    fn classify_passed_su_exit_code_zero() {
        let stats = SuiteStats {
            passed: 5,
            ..Default::default()
        };
        assert_eq!(
            crate::suite_verification::classifica_esito(Some(0), stats.eseguiti()),
            SuiteOutcome::Passed
        );
    }

    #[test]
    fn classify_tests_failed_quando_almeno_un_test_e_eseguito() {
        let stats = SuiteStats {
            passed: 2,
            failed: 1,
            ..Default::default()
        };
        assert_eq!(
            crate::suite_verification::classifica_esito(Some(1), stats.eseguiti()),
            SuiteOutcome::TestsFailed
        );
    }

    #[test]
    fn classify_setup_failed_quando_zero_test_eseguiti() {
        // Caso reale (bacheca-attivita, 31/07/2026): webServer non parte,
        // Playwright esce con errore prima di eseguire un solo test.
        let stats = SuiteStats::default();
        assert_eq!(
            crate::suite_verification::classifica_esito(Some(1), stats.eseguiti()),
            SuiteOutcome::SetupFailed
        );
    }

    #[test]
    fn classify_setup_failed_ignora_skipped_a_zero() {
        // exit_code != 0 con SOLO skipped (nessun passed/failed) e' comunque
        // "eseguito qualcosa": non deve essere classificato SetupFailed.
        let stats = SuiteStats {
            skipped: 3,
            ..Default::default()
        };
        assert_eq!(
            crate::suite_verification::classifica_esito(Some(1), stats.eseguiti()),
            SuiteOutcome::TestsFailed
        );
    }

    #[test]
    fn extract_cause_preferisce_ultima_riga_stderr() {
        let stdout = "Running tests...\n";
        let stderr = "Some warning\nError: Process from config.webServer was not able to start. Exit code: 1\n";
        assert_eq!(
            extract_failure_cause(stdout, stderr).as_deref(),
            Some("Error: Process from config.webServer was not able to start. Exit code: 1")
        );
    }

    #[test]
    fn extract_cause_ripiega_su_stdout_se_stderr_vuoto() {
        let stdout = "avvio...\nError: qualcosa e' andato storto\n";
        assert_eq!(
            extract_failure_cause(stdout, "").as_deref(),
            Some("Error: qualcosa e' andato storto")
        );
    }

    #[test]
    fn extract_cause_none_se_entrambi_vuoti() {
        assert_eq!(extract_failure_cause("", "   \n\n"), None);
    }

    #[test]
    fn extract_cause_tronca_a_300_char() {
        let long_line = "x".repeat(500);
        let cause = extract_failure_cause("", &long_line).unwrap();
        assert_eq!(cause.len(), 300);
    }

    #[test]
    fn summary_setup_failed_non_dice_0_passati_0_falliti() {
        let stats = SuiteStats::default();
        let stderr = "Error: Process from config.webServer was not able to start. Exit code: 1\n";
        let summary = playwright_result_summary(Some(1), &stats, "", stderr);
        assert_eq!(summary.status, "failed");
        assert_eq!(summary.outcome, "setup_failed");
        assert!(!summary.label.contains("0 passati"));
        assert!(summary.msg.contains(
            "Error: Process from config.webServer was not able to start. Exit code: 1"
        ));
        assert_eq!(
            summary.failure_cause.as_deref(),
            Some("Error: Process from config.webServer was not able to start. Exit code: 1")
        );
    }

    #[test]
    fn summary_tests_failed_riporta_conteggi() {
        let stats = SuiteStats {
            passed: 2,
            failed: 1,
            ..Default::default()
        };
        let summary = playwright_result_summary(Some(1), &stats, "", "");
        assert_eq!(summary.status, "failed");
        assert_eq!(summary.outcome, "tests_failed");
        assert_eq!(summary.label, "2 passati, 1 falliti");
        assert_eq!(summary.failure_cause, None);
    }

    #[test]
    fn summary_passed() {
        let stats = SuiteStats {
            passed: 3,
            ..Default::default()
        };
        let summary = playwright_result_summary(Some(0), &stats, "", "");
        assert_eq!(summary.status, "passed");
        assert_eq!(summary.outcome, "passed");
        assert_eq!(summary.failure_cause, None);
    }
}

#[cfg(test)]
mod target_readiness_tests {
    use super::*;
    use crate::project_workspace::service_recovery::{probe_port, PortReadiness};

    /// Una porta su cui nessuno ascolta: porta effimera presa e rilasciata.
    async fn porta_muta() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind effimero");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        port
    }

    /// La causa prodotta dal gate attraversa INTERA la catena del setup_failed
    /// (fix fb5398c2): stessa strada del run vero (`playwright_result_summary`,
    /// regola O), con la risposta della porta nata dal produttore
    /// ([`probe_port`] su una porta muta reale), mai scritta a mano.
    ///
    /// MUTAZIONE: rendendo la causa multiriga, o piu' lunga del taglio dei 300
    /// char di `extract_failure_cause`, `failure_cause` non e' piu' la causa
    /// intera e l'ultima asserzione rosseggia.
    #[tokio::test]
    async fn la_causa_di_non_pronto_attraversa_la_catena_del_setup_failed() {
        let port = porta_muta().await;
        let ready = PortReadiness {
            answer: probe_port(port).await,
            stable: crate::project_workspace::service_recovery::stable_enough(None),
        };
        assert!(!ready.ready(), "porta muta: il gate non puo' dirla pronta");

        let cause = target_not_ready_cause(
            &ready,
            i32::from(port),
            "demo-frontend.service",
            Duration::from_secs(60),
        );
        assert!(
            cause.contains("Servizio non pronto") && cause.contains("MUTA"),
            "la causa dice cosa manca e cosa ha risposto l'ultima osservazione: {cause}"
        );

        let summary =
            playwright_result_summary(Some(-1), &SuiteStats::default(), "", &cause);
        assert_eq!(summary.outcome, "setup_failed");
        assert_eq!(
            summary.failure_cause.as_deref(),
            Some(cause.as_str()),
            "la causa deve arrivare INTERA nel failure_cause del job"
        );
    }

    /// Semina il minimo per una allocazione di porta nel DB meta migrato
    /// (FK: teams -> users -> projects) e ritorna il project_id.
    async fn seed_project(pool: &sqlx::PgPool) -> Uuid {
        let team = Uuid::new_v4();
        let user = Uuid::new_v4();
        let project = Uuid::new_v4();
        sqlx::query("INSERT INTO teams (id, name, slug) VALUES ($1,'T',$2)")
            .bind(team)
            .bind(team.to_string())
            .execute(pool)
            .await
            .expect("team");
        sqlx::query("INSERT INTO users (id, email, display_name) VALUES ($1,$2,'U')")
            .bind(user)
            .bind(format!("{user}@t.local"))
            .execute(pool)
            .await
            .expect("user");
        sqlx::query(
            "INSERT INTO projects (id, team_id, name, slug, owner_user_id) \
             VALUES ($1,$2,'P',$3,$4)",
        )
        .bind(project)
        .bind(team)
        .bind(project.to_string())
        .bind(user)
        .execute(pool)
        .await
        .expect("project");
        project
    }

    async fn seed_allocation(
        pool: &sqlx::PgPool,
        project: Uuid,
        port: u16,
        service_unit: Option<&str>,
    ) {
        sqlx::query(
            "INSERT INTO nexus_port_allocations (project_id, port, label, service_unit) \
             VALUES ($1, $2, 'frontend', $3)",
        )
        .bind(project)
        .bind(i32::from(port))
        .bind(service_unit)
        .execute(pool)
        .await
        .expect("allocazione");
    }

    /// IL CASO DEL MANDATO: la porta bersaglio appartiene a una unit di
    /// servizio e nessuno la serve — il gate deve produrre la causa (e quindi
    /// il setup_failed), MAI lasciar partire la suite. Attraversa la strada
    /// intera della produzione (regola O): query dell'unit, finestra dal
    /// setting in DB (la migrazione 0662 e' nel migrator embedded; qui
    /// azzerata per non attendere 60 secondi veri), ciclo `await_port_ready`
    /// su una porta muta reale.
    ///
    /// MUTAZIONE: rompendo l'attesa — un gate che ritorna `None` senza provare
    /// la porta, com'era il runner prima del fix — questo test rosseggia.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_gate_su_porta_muta_di_una_unit_produce_la_causa(pool: sqlx::PgPool) {
        let project = seed_project(&pool).await;
        let port = porta_muta().await;
        seed_allocation(&pool, project, port, Some("demo-frontend.service")).await;
        crate::test_support::seed_setting(&pool, PLAYWRIGHT_READINESS_KEY, "0").await;

        let base_url = format!("http://localhost:{port}");
        let cause = await_target_service_ready(&pool, project, Some(&base_url)).await;
        let cause = cause.expect("porta muta di una unit: il gate deve fermare il lancio");
        assert!(
            cause.contains("Servizio non pronto") && cause.contains("demo-frontend.service"),
            "la causa nomina la unit che non risponde: {cause}"
        );
    }

    /// L'ANTI-REGRESSIONE del gate: una porta senza unit legata non ha alcun
    /// contratto da attendere — e' il caso del `webServer` che la suite stessa
    /// avvia, la cui porta risponde solo DOPO il lancio. Il gate deve lasciar
    /// partire la suite subito, anche se la porta ora e' muta.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_gate_senza_unit_legata_non_attende(pool: sqlx::PgPool) {
        let project = seed_project(&pool).await;
        let port = porta_muta().await;
        seed_allocation(&pool, project, port, None).await;

        let base_url = format!("http://localhost:{port}");
        assert_eq!(
            await_target_service_ready(&pool, project, Some(&base_url)).await,
            None,
            "senza unit legata non c'e' contratto: la suite parte (webServer della config)"
        );
        assert_eq!(
            await_target_service_ready(&pool, project, None).await,
            None,
            "senza BASE_URL non c'e' nemmeno una porta da provare"
        );
    }
}
