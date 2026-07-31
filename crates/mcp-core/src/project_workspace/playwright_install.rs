//! Endpoint REST atomico per installare Playwright in un progetto.
//!
//! Fix M19 (richiesta esplicita utente durante test maturita 2026-05-14T1556):
//! > "L'installazione di playwright deve avvenire tramite mcp nexus in modo da
//! >  gestirne la configurazione"
//!
//! POST /api/projects/:id/services/install-playwright
//!
//! Body opzionale:
//! - `target_dir`: subdir relativa al progetto dove installare (default: rileva da package.json
//!   il subpackage con dependencies React/Vite/Next, fallback project_root)
//! - `force`: se true rimuove playwright.config.ts esistenti prima di installare (default false)
//!
//! Operazioni atomiche eseguite:
//! 1. Identifica frontend dir (target_dir | auto-detect | project_root)
//! 2. npm install -D @playwright/test
//! 3. npx playwright install chromium (senza --with-deps per evitare sudo)
//! 4. Legge nexus_port_allocations con pick_dev_port (label dev/app/http/web/frontend/serve/server)
//! 5. Genera playwright.config.ts deterministico con baseURL = http://localhost:<dev_port>
//! 6. Crea e2e/smoke.spec.ts (root render + no console errors)
//! 7. INSERT/UPDATE in settings: key `project:<pid>:playwright_enabled` = "true"
//!
//! Ritorna JSON con: target_dir, dev_port, config_path, smoke_test_path, packages_installed.

use super::*;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const INSTALL_TIMEOUT_SECS: u64 = 600;

/// Sceglie la porta dev tra le allocations del progetto.
/// Priorita: label contains "dev" > "app" > "http" > "web" > "frontend" > "serve" > "server".
/// Esclude esplicitamente label che contengono "backend" / "api" / "fastify" / "express".
/// Fallback: porte >= 5000 (dev typical), poi porta minore tra non-backend, poi 5173 (Vite default).
fn pick_dev_port(allocations: &[(i32, String)]) -> i32 {
    let dev_kw = [
        "dev", "app", "http", "web", "frontend", "serve", "server", "vite", "next", "react",
    ];
    let backend_kw = [
        "backend",
        "api",
        "fastify",
        "express",
        "server-api",
        "dotnet",
        "graphql",
    ];

    let is_backend = |label: &str| {
        backend_kw
            .iter()
            .any(|bk| label.to_lowercase().contains(bk))
    };

    for kw in &dev_kw {
        if let Some((port, _)) = allocations
            .iter()
            .find(|(_, l)| l.to_lowercase().contains(kw) && !is_backend(l))
        {
            return *port;
        }
    }

    // Tra non-backend, prefer porte >= 5000
    let non_backend: Vec<_> = allocations.iter().filter(|(_, l)| !is_backend(l)).collect();
    if let Some((port, _)) = non_backend
        .iter()
        .filter(|(p, _)| *p >= 5000)
        .min_by_key(|(p, _)| *p)
    {
        return *port;
    }
    if let Some((port, _)) = non_backend.iter().min_by_key(|(p, _)| *p) {
        return *port;
    }

    5173 // Vite default
}

/// Rileva la directory frontend cercando un package.json con dipendenze React/Vite/Next.
/// Se non trovato, ritorna `root` (progetto monolitico).
async fn detect_frontend_dir(root: &Path) -> PathBuf {
    let frontend_signals = ["react", "vite", "next", "@vitejs/plugin-react"];

    // Cerca subdir con package.json contenente uno dei signal
    for entry_name in &["frontend", "client", "web", "app", "ui"] {
        let candidate = root.join(entry_name);
        let pkg = candidate.join("package.json");
        if pkg.is_file() {
            if let Ok(content) = tokio::fs::read_to_string(&pkg).await {
                if frontend_signals.iter().any(|s| content.contains(s)) {
                    return candidate;
                }
            }
        }
    }

    // Fallback: root stesso, se ha package.json
    let root_pkg = root.join("package.json");
    if root_pkg.is_file() {
        return root.to_path_buf();
    }

    // Ultimo fallback: root (l'agente dovra gestire l'errore di install)
    root.to_path_buf()
}

/// Esegue un comando con timeout, ritorna (stdout, stderr, exit_code).
async fn run_with_timeout(
    cmd: &str,
    args: &[&str],
    cwd: &PathBuf,
    timeout_secs: u64,
) -> Result<(String, String, i32), String> {
    let mut child = Command::new(cmd)
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn {}: {}", cmd, e))?;

    let mut stdout_buf = String::new();
    let mut stderr_buf = String::new();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let exit_status =
        tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), child.wait())
            .await
            .map_err(|_| format!("timeout dopo {}s eseguendo {}", timeout_secs, cmd))?
            .map_err(|e| format!("wait {}: {}", cmd, e))?;

    if let Some(mut s) = stdout {
        let _ = s.read_to_string(&mut stdout_buf).await;
    }
    if let Some(mut s) = stderr {
        let _ = s.read_to_string(&mut stderr_buf).await;
    }

    Ok((stdout_buf, stderr_buf, exit_status.code().unwrap_or(-1)))
}

/// POST /api/projects/:id/services/install-playwright
pub async fn install_playwright(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;

    if !context.access.can_write {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Non hai permessi di scrittura su questo progetto",
        ));
    }

    let force = body.get("force").and_then(Value::as_bool).unwrap_or(false);
    let target_override = body.get("target_dir").and_then(Value::as_str);

    let root = context.root_path.clone();

    let target_dir = if let Some(t) = target_override {
        let p = root.join(t);
        if !p.starts_with(&root) {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "target_dir fuori dalla root del progetto",
            ));
        }
        p
    } else {
        detect_frontend_dir(&root).await
    };

    if !target_dir.is_dir() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            format!("target_dir non esiste: {}", target_dir.display()),
        ));
    }

    // ── 1. Force cleanup di config esistenti se richiesto ────────────────
    if force {
        let _ = tokio::fs::remove_file(target_dir.join("playwright.config.ts")).await;
        let _ = tokio::fs::remove_file(target_dir.join("playwright.config.js")).await;
        let _ = tokio::fs::remove_file(target_dir.join("playwright.config.mjs")).await;
    }

    // ── 2. npm install -D @playwright/test ────────────────────────────────
    let install_result = run_with_timeout(
        "npm",
        &["install", "-D", "@playwright/test"],
        &target_dir,
        INSTALL_TIMEOUT_SECS,
    )
    .await
    .map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("npm install fallito: {}", e),
        )
    })?;

    if install_result.2 != 0 {
        return Err(api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "npm install -D @playwright/test fallito (exit {}): {}",
                install_result.2,
                install_result
                    .1
                    .lines()
                    .take(10)
                    .collect::<Vec<_>>()
                    .join(" | ")
            ),
        ));
    }

    // ── 3. npx playwright install chromium ───────────────────────────────
    // Strategia:
    //   3a. Detect passwordless sudo (sudo -n true). Se OK, prova
    //       `playwright install --with-deps chromium` per installare anche le
    //       system libs (libnspr4, libnss3, ...).
    //   3b. Altrimenti, fallback: `playwright install chromium` (solo browser).
    //   3c. Post-check: ldd sull'eseguibile chrome-headless-shell per
    //       scoprire libs runtime mancanti. Se trovate, ritorna nel response
    //       una sezione `manual_install_required` con il comando apt esatto.
    //
    // Fix 31/05/2026: il pannello Nexus "Abilita Playwright" segnalava
    // 'Failed to install browsers Error: exit code 1' senza spiegare cosa
    // fare. Ora se sudo non e' passwordless e mancano libs sistema, l'UI
    // ottiene istruzioni esecutive copy-paste.
    let passwordless_sudo = run_with_timeout("sudo", &["-n", "true"], &target_dir, 5)
        .await
        .map(|(_, _, code)| code == 0)
        .unwrap_or(false);

    let browser_result = if passwordless_sudo {
        run_with_timeout(
            "sudo",
            &[
                "-n",
                "npx",
                "playwright",
                "install",
                "--with-deps",
                "chromium",
            ],
            &target_dir,
            INSTALL_TIMEOUT_SECS,
        )
        .await
    } else {
        run_with_timeout(
            "npx",
            &["playwright", "install", "chromium"],
            &target_dir,
            INSTALL_TIMEOUT_SECS,
        )
        .await
    };
    let browser_result = browser_result.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("playwright install fallito: {}", e),
        )
    })?;

    let browser_status = browser_result.2;

    // Post-check: rileva libs sistema mancanti tramite ldd sull'eseguibile.
    let missing_libs = detect_missing_chromium_libs().await;
    let manual_install_required = !missing_libs.is_empty() && !passwordless_sudo;
    let apt_command = if manual_install_required {
        Some(format!(
            "sudo apt-get update && sudo apt-get install -y {}",
            chromium_apt_packages_for(&missing_libs).join(" ")
        ))
    } else {
        None
    };

    // ── 4. Leggi port allocations e scegli dev port ──────────────────────
    let port_rows =
        sqlx::query("SELECT port, label FROM nexus_port_allocations WHERE project_id=$1")
            .bind(project_id)
            .fetch_all(&state.db)
            .await
            .map_err(|e| {
                api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("DB query ports: {}", e),
                )
            })?;

    let allocations: Vec<(i32, String)> = port_rows
        .iter()
        .map(|r| (r.get::<i32, _>("port"), r.get::<String, _>("label")))
        .collect();

    let dev_port = pick_dev_port(&allocations);
    let base_url = format!("http://localhost:{}", dev_port);

    // ── 5. Genera playwright.config.ts deterministico ────────────────────
    let config_path = target_dir.join("playwright.config.ts");
    let config_content = format!(
        r#"import {{ defineConfig, devices }} from '@playwright/test';

// Generato da Nexus install-playwright (Fix M19)
// dev_port: {} (da nexus_port_allocations, pick_dev_port)
// Per override usa BASE_URL env var
export default defineConfig({{
  testDir: './e2e',
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 1,
  workers: 1,
  reporter: 'list',
  timeout: 30_000,
  use: {{
    baseURL: process.env.BASE_URL || process.env.PLAYWRIGHT_BASE_URL || '{}',
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
  }},
  projects: [
    {{ name: 'chromium', use: {{ ...devices['Desktop Chrome'] }} }},
  ],
  // webServer assente per scelta: il servizio dev lo gestisce Nexus (pannello
  // Servizi / nexus_port_allocations), non Playwright. Il pulsante chat
  // "Abilita Playwright" (apps/web-ide/lib/chat-prompts.ts::promptEnablePlaywright)
  // detta lo stesso config per il canale di installazione via agente: se questa
  // filosofia cambia, allineare anche li'.
  webServer: undefined,
}});
"#,
        dev_port, base_url
    );

    tokio::fs::write(&config_path, &config_content)
        .await
        .map_err(|e| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("scrittura config: {}", e),
            )
        })?;

    // ── 6. Crea e2e/smoke.spec.ts ────────────────────────────────────────
    let e2e_dir = target_dir.join("e2e");
    tokio::fs::create_dir_all(&e2e_dir).await.map_err(|e| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("mkdir e2e: {}", e),
        )
    })?;

    let smoke_path = e2e_dir.join("smoke.spec.ts");
    let smoke_exists = smoke_path.is_file();
    if !smoke_exists {
        let smoke_content = r#"import { test, expect } from '@playwright/test';

// Smoke test generato da Nexus install-playwright (Fix M19).
// Verifica che la root dell'app risponda e non emetta errori console JS.
test('app root risponde senza errori console', async ({ page }) => {
  const consoleErrors: string[] = [];
  page.on('console', (msg) => {
    if (msg.type() === 'error') consoleErrors.push(msg.text());
  });

  await page.goto('/');
  await page.waitForLoadState('networkidle');

  // L'app puo redirect a /login o servire la home: entrambi accettabili
  expect(page.url()).toMatch(/^http/);

  // Nessun errore console critico al primo render
  expect(consoleErrors).toEqual([]);
});
"#;
        tokio::fs::write(&smoke_path, smoke_content)
            .await
            .map_err(|e| {
                api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("scrittura smoke: {}", e),
                )
            })?;
    }

    // ── 7. INSERT in settings: playwright_enabled per il progetto ────────
    let setting_key = format!("project:{}:playwright_enabled", project_id);
    let _ = sqlx::query(
        r#"
        INSERT INTO settings (key, value, category, description, is_secret, updated_at)
        VALUES ($1, 'true', 'project', 'Playwright abilitato e configurato', FALSE, NOW())
        ON CONFLICT (key) DO UPDATE
          SET value = 'true', updated_at = NOW()
        "#,
    )
    .bind(&setting_key)
    .execute(&state.db)
    .await;

    // ── Ritorna stato dettagliato ────────────────────────────────────────
    let relative_target = target_dir
        .strip_prefix(&root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| target_dir.to_string_lossy().to_string());

    Ok(Json(json!({
        "ok": missing_libs.is_empty(),
        "target_dir": relative_target,
        "dev_port": dev_port,
        "base_url": base_url,
        "config_path": config_path.strip_prefix(&root).map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
        "smoke_test_path": smoke_path.strip_prefix(&root).map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
        "smoke_already_existed": smoke_exists,
        "force_applied": force,
        "browser_install_exit_code": browser_status,
        "passwordless_sudo_available": passwordless_sudo,
        "missing_system_libs": missing_libs,
        "manual_install_required": manual_install_required,
        "apt_command": apt_command,
        "manual_install_hint": if manual_install_required {
            Some(format!(
                "Le libs sistema per Chromium mancano e sudo non e' passwordless. \
                 Esegui MANUALMENTE nel terminale WSL: '{}'  - poi riprova Abilita Playwright. \
                 Alternativa: configura sudoers passwordless per i pacchetti apt.",
                apt_command.as_deref().unwrap_or("")
            ))
        } else { None },
        "port_allocations_count": allocations.len(),
        "available_allocations": allocations.iter().map(|(p, l)| json!({"port": p, "label": l})).collect::<Vec<_>>(),
        "setting_key": setting_key,
    })))
}

/// Sceglie l'eseguibile da ispezionare dentro una dir di revisione chromium.
/// ORDINE: prima il Chromium COMPLETO (headed) reale `chrome-linux64/chrome`
/// (usato da visual_compare e @playwright/mcp), poi l'headless shell. Il vecchio
/// `chrome-linux/chrome` non esiste piu' nelle build Playwright correnti: era il
/// bug d1 (preflight cieco al chromium completo). Funzione pura (testabile).
fn pick_chromium_executable(chromium_dir: &Path) -> Option<PathBuf> {
    const CANDIDATES: [&str; 3] = [
        "chrome-linux64/chrome",
        "chrome-headless-shell-linux64/chrome-headless-shell",
        "chrome-linux/chrome",
    ];
    CANDIDATES
        .iter()
        .map(|c| chromium_dir.join(c))
        .find(|p| std::fs::metadata(p).map(|m| m.is_file()).unwrap_or(false))
}

/// Detect chromium runtime libs mancanti via `ldd` sul chrome-headless-shell.
/// Restituisce vec di nomi `.so` non risolti (es. "libnspr4.so", "libnss3.so").
/// Empty vec = tutto OK.
async fn detect_missing_chromium_libs() -> Vec<String> {
    let cache_root = match std::env::var("HOME") {
        Ok(h) => format!("{}/.cache/ms-playwright", h),
        Err(_) => return Vec::new(),
    };
    let glob_root = match tokio::fs::read_dir(&cache_root).await {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    // Trova la dir chromium con la revisione piu' alta. Prima si prendeva il
    // PRIMO "chromium*" in ordine di read_dir (non deterministico), includendo
    // anche le dir "chromium_headless_shell-*" che NON contengono il browser
    // completo: il preflight finiva per controllare la dir sbagliata. Ora si
    // ordina per revisione numerica decrescente, considerando sia il browser
    // completo (chromium-*) sia l'headless shell (chromium_headless_shell-*).
    let mut chromium_dirs: Vec<(u64, PathBuf)> = Vec::new();
    let mut iter = glob_root;
    while let Ok(Some(entry)) = iter.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        let rev = name
            .strip_prefix("chromium_headless_shell-")
            .or_else(|| name.strip_prefix("chromium-"))
            .and_then(|s| s.parse::<u64>().ok());
        if let Some(rev) = rev {
            chromium_dirs.push((rev, entry.path()));
        }
    }
    chromium_dirs.sort_by_key(|(rev, _)| std::cmp::Reverse(*rev));
    let chromium_dir = match chromium_dirs.into_iter().next() {
        Some((_, p)) => p,
        None => return Vec::new(),
    };
    let exe = match pick_chromium_executable(&chromium_dir) {
        Some(p) => p,
        None => return Vec::new(),
    };
    // Esegui ldd
    let output = match Command::new("ldd").arg(&exe).output().await {
        Ok(o) => {
            String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)
        }
        Err(_) => return Vec::new(),
    };
    let mut missing = Vec::new();
    for line in output.lines() {
        if line.contains("not found") {
            // Es: "	libnspr4.so => not found" -> estrai libnspr4.so
            if let Some(name) = line.trim().split("=>").next().map(str::trim) {
                if name.ends_with(".so") || name.contains(".so.") {
                    missing.push(name.to_string());
                }
            }
        }
    }
    missing
}

/// Mappa i `.so` mancanti ai pacchetti apt corrispondenti (Ubuntu/Debian).
/// La lista deriva dalla doc Playwright `playwright install-deps`.
fn chromium_apt_packages_for(missing: &[String]) -> Vec<&'static str> {
    let mut pkgs: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
    for m in missing {
        let lib = m.split('.').next().unwrap_or(m);
        match lib {
            "libnspr4" => {
                pkgs.insert("libnspr4");
            }
            "libnss3" | "libnssutil3" | "libsmime3" => {
                pkgs.insert("libnss3");
            }
            "libatk-bridge-2" | "libatk-1" => {
                pkgs.insert("libatk-bridge2.0-0");
                pkgs.insert("libatk1.0-0");
            }
            "libxkbcommon" => {
                pkgs.insert("libxkbcommon0");
            }
            "libxcomposite" => {
                pkgs.insert("libxcomposite1");
            }
            "libxdamage" => {
                pkgs.insert("libxdamage1");
            }
            "libxfixes" => {
                pkgs.insert("libxfixes3");
            }
            "libxrandr" => {
                pkgs.insert("libxrandr2");
            }
            "libgbm" => {
                pkgs.insert("libgbm1");
            }
            "libasound" => {
                pkgs.insert("libasound2t64");
            }
            "libcairo" => {
                pkgs.insert("libcairo2");
            }
            "libpango-1" => {
                pkgs.insert("libpango-1.0-0");
            }
            "libdrm" => {
                pkgs.insert("libdrm2");
            }
            "libcups" => {
                pkgs.insert("libcups2");
            }
            "libatspi" => {
                pkgs.insert("libatspi2.0-0");
            }
            _ => {}
        }
    }
    // Default sicuro: se anche un singolo .so manca, aggiungo set completo
    // (apt e' idempotent, costa poco).
    if !missing.is_empty() {
        for p in [
            "libnspr4",
            "libnss3",
            "libatk-bridge2.0-0",
            "libatk1.0-0",
            "libxkbcommon0",
            "libxcomposite1",
            "libxdamage1",
            "libxfixes3",
            "libxrandr2",
            "libgbm1",
            "libasound2t64",
            "libcairo2",
            "libpango-1.0-0",
            "libdrm2",
            "libcups2",
            "libatspi2.0-0",
        ] {
            pkgs.insert(p);
        }
    }
    let mut v: Vec<&'static str> = pkgs.into_iter().collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// d1: il preflight deve riconoscere il Chromium COMPLETO reale in
    /// 'chrome-linux64/chrome' (prima era cieco a quel path: controllava solo
    /// 'chrome-linux/chrome' obsoleto e l'headless shell).
    #[test]
    fn pick_chromium_executable_finds_full_chrome_linux64() {
        let tmp = std::env::temp_dir().join(format!("nexus_pw_install_{}", uuid::Uuid::new_v4()));
        let exe_dir = tmp.join("chrome-linux64");
        fs::create_dir_all(&exe_dir).unwrap();
        fs::write(exe_dir.join("chrome"), b"x").unwrap();

        let got = pick_chromium_executable(&tmp).expect("deve trovare il chromium completo");
        assert!(
            got.ends_with("chrome-linux64/chrome"),
            "selezionato il path sbagliato: {got:?}"
        );

        fs::remove_dir_all(&tmp).ok();
    }

    /// Se manca il browser completo ma c'e' l'headless shell, ripiega su quello.
    #[test]
    fn pick_chromium_executable_falls_back_to_headless_shell() {
        let tmp = std::env::temp_dir().join(format!("nexus_pw_install_{}", uuid::Uuid::new_v4()));
        let exe_dir = tmp.join("chrome-headless-shell-linux64");
        fs::create_dir_all(&exe_dir).unwrap();
        fs::write(exe_dir.join("chrome-headless-shell"), b"x").unwrap();

        let got = pick_chromium_executable(&tmp).expect("deve trovare l'headless shell");
        assert!(
            got.ends_with("chrome-headless-shell-linux64/chrome-headless-shell"),
            "selezionato il path sbagliato: {got:?}"
        );

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn pick_chromium_executable_none_when_empty() {
        let tmp = std::env::temp_dir().join(format!("nexus_pw_install_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        assert!(pick_chromium_executable(&tmp).is_none());
        fs::remove_dir_all(&tmp).ok();
    }
}
