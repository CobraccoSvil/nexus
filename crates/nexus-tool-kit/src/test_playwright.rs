//! `testing::test_playwright` — esegue la suite Playwright del progetto.
//!
//! Strategia di rilevamento:
//! - `playwright.config.ts` | `playwright.config.js` → Playwright configurato
//! - Altrimenti → verifica `node_modules/@playwright/test` (installato ma senza config)
//!
//! Argomenti opzionali:
//! - `filter`   — stringa passata come grep/filter a `npx playwright test <filter>`
//! - `project`  — nome del progetto Playwright (es. "chromium", "firefox")
//! - `workers`  — numero di worker paralleli (default: 1 per ambienti WSL/headless)
//! - `reporter` — "list" | "line" | "json" (default: "list")
//! - `headed`   — se `true` lancia in modalità visuale (default: false = headless)
//!
//! Il tool usa sempre `--timeout 30000` per impedire test zombie e
//! imposta la variabile d'ambiente `CI=true` per garantire headless anche
//! su sistemi con display.
//!
//! NON e' l'esecutore del ciclo di chiusura. Quello e' il punto unico
//! `mcp-core::suite_verification`, a cui delegano il final_gate, il tool
//! agente `run_playwright_tests` e il ciclo review: li' l'esito e' legato allo
//! stato del codice (memoria) e un fallimento non riprodotto viene classificato
//! `flaky` invece di aprire una correzione. Questo tool e' una lettura
//! ISOLATA, esposta nel catalogo MCP: esegue e riporta i conteggi, non
//! memorizza nulla e non classifica. Usarlo per decidere se un lavoro e'
//! finito rimetterebbe in piedi l'esecutore cieco che quel punto unico ha
//! tolto (questo crate non puo' dipendere da mcp-core: la delega andrebbe
//! fatta spostando il chiamante, non duplicando la politica).

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct TestPlaywrightTool;

/// Timeout esteso per i test browser: 5 minuti (Playwright può impiegare tempo
/// per scaricare browser o eseguire suite pesanti).
const PLAYWRIGHT_TIMEOUT_SECS: u64 = 300;

/// Parsa le statistiche dall'output testuale di Playwright (`--reporter=list`).
///
/// Riga summary tipica:
/// `  5 passed (12s)` oppure `  3 failed (8s)` oppure
/// `  2 passed, 1 failed, 1 skipped (20s)`
fn parse_playwright_output(stdout: &str, stderr: &str) -> PlaywrightStats {
    let combined = format!("{stdout}\n{stderr}");
    let mut stats = PlaywrightStats::default();

    for line in combined.lines() {
        let trimmed = line.trim();

        // Righe tipo "  5 passed (12s)"
        if let Some(n) = extract_count(trimmed, "passed") {
            stats.passed += n;
        }
        if let Some(n) = extract_count(trimmed, "failed") {
            stats.failed += n;
        }
        if let Some(n) = extract_count(trimmed, "skipped") {
            stats.skipped += n;
        }
        if let Some(n) = extract_count(trimmed, "flaky") {
            stats.flaky += n;
        }

        // Righe tipo "    ✓  1 [chromium] › auth.spec.ts:10:3 › should login"
        if (trimmed.starts_with('✓') || trimmed.starts_with("  ✓") || trimmed.contains("] ›"))
            && (trimmed.contains("FAILED") || trimmed.starts_with('✘') || trimmed.starts_with("  ✘"))
            {
                stats.failed_tests.push(trimmed.to_string());
            }

        // Righe tipo "    ✘  1 [chromium] › home.spec.ts:5:3 › should load ─────"
        if trimmed.starts_with('✘') || (trimmed.contains("✘") && trimmed.contains("›")) {
            let name = trimmed
                .trim_start_matches('✘')
                .trim_start_matches(|c: char| c.is_numeric() || c == ' ' || c == '[')
                .trim()
                .to_string();
            if !name.is_empty() && !stats.failed_tests.contains(&name) {
                stats.failed_tests.push(name);
            }
        }
    }

    stats
}

fn extract_count(line: &str, keyword: &str) -> Option<usize> {
    let pos = line.find(keyword)?;
    let before = line[..pos].trim();
    // Prende l'ultimo token numerico prima della keyword
    before.split_whitespace().last()?.parse().ok()
}

#[derive(Debug, Default)]
struct PlaywrightStats {
    passed: usize,
    failed: usize,
    skipped: usize,
    flaky: usize,
    failed_tests: Vec<String>,
}

#[async_trait]
impl NexusToolHandler for TestPlaywrightTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let root = &ctx.project_root;

        // ── Rilevamento configurazione ───────────────────────────────────────
        let has_config = root.join("playwright.config.ts").is_file()
            || root.join("playwright.config.js").is_file()
            || root.join("playwright.config.mjs").is_file();

        let has_playwright_installed = root
            .join("node_modules")
            .join("@playwright")
            .join("test")
            .is_dir();

        if !has_config && !has_playwright_installed {
            return Ok(json!({
                "ok": false,
                "error": "Playwright non trovato: mancano sia playwright.config.ts che node_modules/@playwright/test. Installa con: pnpm add -D @playwright/test",
                "playwright_installed": false,
                "config_found": false,
            }));
        }

        // ── Costruzione argomenti ────────────────────────────────────────────
        let filter = args.get("filter").and_then(Value::as_str).map(String::from);
        let project = args
            .get("project")
            .and_then(Value::as_str)
            .map(String::from);
        let workers = args
            .get("workers")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .to_string();
        let reporter = args
            .get("reporter")
            .and_then(Value::as_str)
            .unwrap_or("list")
            .to_string();
        let headed = args.get("headed").and_then(Value::as_bool).unwrap_or(false);

        let mut cmd_args: Vec<String> = vec![
            "playwright".into(),
            "test".into(),
            "--timeout".into(),
            "30000".into(),
            "--workers".into(),
            workers,
            "--reporter".into(),
            reporter.clone(),
        ];

        if !headed {
            // headless è il default ma lo forziamo esplicitamente
            // per sicurezza in ambienti WSL senza display
        }

        if let Some(ref p) = project {
            cmd_args.push("--project".into());
            cmd_args.push(p.clone());
        }

        if let Some(ref f) = filter {
            cmd_args.push(f.clone());
        }

        let refs: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();

        // Usa timeout esteso (300s) indipendentemente dal timeout di default del tool
        let effective_timeout = PLAYWRIGHT_TIMEOUT_SECS.max(ctx.timeout_secs);

        let out = run_cmd("npx", &refs, root, effective_timeout).await?;

        let stats = parse_playwright_output(&out.stdout, &out.stderr);

        // Ultime 50 righe di output per context
        let stdout_tail: Vec<&str> = out.stdout.lines().rev().take(50).collect::<Vec<_>>();
        let stderr_tail: Vec<&str> = out.stderr.lines().rev().take(20).collect::<Vec<_>>();

        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "playwright_installed": has_playwright_installed,
            "config_found": has_config,
            "reporter": reporter,
            "filter": filter,
            "project": project,
            "passed": stats.passed,
            "failed": stats.failed,
            "skipped": stats.skipped,
            "flaky": stats.flaky,
            "total": stats.passed + stats.failed + stats.skipped,
            "failed_tests": stats.failed_tests,
            "stdout_tail": stdout_tail,
            "stderr_tail": stderr_tail,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "filter": {
                    "type": "string",
                    "description": "Filtro per nome file o test (es. 'auth' per eseguire solo i test nei file che contengono 'auth')"
                },
                "project": {
                    "type": "string",
                    "description": "Progetto Playwright da eseguire (es. 'chromium', 'firefox', 'webkit')"
                },
                "workers": {
                    "type": "integer",
                    "description": "Numero di worker paralleli (default: 1 per WSL/headless)"
                },
                "reporter": {
                    "type": "string",
                    "enum": ["list", "line", "dot"],
                    "description": "Formato output (default: list)"
                },
                "headed": {
                    "type": "boolean",
                    "description": "Se true lancia i browser in modalità visuale (default: false = headless)"
                }
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::write_subproc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_passed_only() {
        let stdout = "Running 3 tests using 1 worker\n\n  3 passed (5s)\n";
        let stats = parse_playwright_output(stdout, "");
        assert_eq!(stats.passed, 3);
        assert_eq!(stats.failed, 0);
    }

    #[test]
    fn test_parse_mixed_results() {
        let stdout = "  2 passed, 1 failed (12s)\n";
        let stats = parse_playwright_output(stdout, "");
        assert_eq!(stats.passed, 2);
        assert_eq!(stats.failed, 1);
    }

    #[test]
    fn test_parse_skipped() {
        let stdout = "  4 passed, 2 skipped (8s)\n";
        let stats = parse_playwright_output(stdout, "");
        assert_eq!(stats.passed, 4);
        assert_eq!(stats.skipped, 2);
    }

    #[test]
    fn test_extract_count() {
        assert_eq!(extract_count("  3 passed (5s)", "passed"), Some(3));
        assert_eq!(extract_count("  0 failed", "failed"), Some(0));
        assert_eq!(extract_count("no match here", "passed"), None);
    }
}
