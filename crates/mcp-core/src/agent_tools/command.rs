//! Tool comandi shell: run_command (con auto-routing long-running) e run_tests.

use super::*;

/// Durata del "probe" per rilevare comandi long-running non noti.
/// Se il processo non termina entro questo tempo, viene killato e ri-lanciato nel terminale.
const RUN_COMMAND_PROBE_SECS: u64 = 10;

const RUN_TESTS_DEFAULT_TIMEOUT: u64 = 120;
const RUN_TESTS_MAX_TIMEOUT: u64 = 300;

/// Progetto con DB registrato e `allow_ddl_override = false` (default): schema change solo via migration.
async fn strict_migration_only_project(ctx: &AgentToolContext) -> bool {
    match sqlx::query_scalar::<_, bool>(
        "SELECT allow_ddl_override FROM project_database_config WHERE project_id = $1 LIMIT 1",
    )
    .bind(ctx.project_id)
    .fetch_optional(&*ctx.db)
    .await
    {
        Ok(Some(false)) => true,
        _ => false,
    }
}

fn shell_command_bypasses_migration_policy(cmd: &str) -> bool {
    let c = cmd.to_lowercase();
    c.contains("flyway")
        || c.contains("liquibase")
        || c.contains("alembic upgrade")
        || c.contains("alembic downgrade")
        || c.contains("prisma migrate")
        || c.contains("dotnet ef database update")
        || c.contains("sqlx migrate")
        || c.contains("knex migrate")
        || c.contains("manage.py migrate")
        || c.contains("rails db:migrate")
        || c.contains("rake db:migrate")
        || (c.contains("-f ") && (c.contains("migrat") || c.contains("/migrations/") || c.contains("\\migrations\\")))
}

fn shell_looks_like_sql_cli_with_ddl(cmd: &str) -> bool {
    let lower = cmd.to_lowercase();
    let sql_cli = lower.contains("psql")
        || lower.contains("sqlcmd")
        || lower.contains("sqlite3")
        || lower.starts_with("mysql")
        || lower.contains(" mysql ")
        || lower.contains("/mysql ")
        || lower.contains("mysql -");
    if !sql_cli {
        return false;
    }
    crate::nexus_tools::db_helper::contains_ddl_statement(cmd)
}

pub(super) async fn tool_run_command(ctx: &AgentToolContext, input: &Value) -> String {
    let command = match input.get("command").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => return "[Errore: parametro 'command' mancante]".to_string(),
    };
    if command.trim().is_empty() {
        return "[Errore: comando vuoto]".to_string();
    }

    // ── Livello 0 GUARDRAIL: blocca comandi infrastruttura-distruttivi ──
    // Difesa in profondita': blacklist server-side che non puo' essere
    // bypassata dal prompt utente / jailbreak. Vedi safety.rs per la lista
    // pattern (psql -d nexus, prisma migrate reset, docker exec ideai-*,
    // DROP/TRUNCATE/DELETE su tabelle Nexus, rm -rf su /home/administrator/ideai).
    if let Some(reason) = super::safety::check_command(&command) {
        tracing::warn!(
            "SECURITY_GUARDRAIL: comando BLOCCATO category={} project_id={} cmd_excerpt={:?}",
            reason.category,
            ctx.project_id,
            command.chars().take(160).collect::<String>(),
        );
        let _ = persist_security_audit(ctx, &command, &reason).await;
        // PR hardening: audit trail centralizzato (oltre al log security_audit esistente)
        crate::security::record_audit(
            crate::security::AuditEntry::blocked(ctx.project_id, "command_blocked", "command")
                .with_resource(reason.category.to_string())
                .with_details(serde_json::json!({
                    "command_excerpt": command.chars().take(200).collect::<String>(),
                    "reason": reason.message,
                })),
        );
        return super::safety::format_blocked_result(&command, &reason);
    }

    if strict_migration_only_project(ctx).await
        && !shell_command_bypasses_migration_policy(&command)
        && shell_looks_like_sql_cli_with_ddl(&command)
    {
        return format!(
            "[BLOCCATO — policy database progetto]\n\
             Questo progetto richiede modifiche di schema solo tramite migration versionate (file nel repo + registro Nexus). Non eseguire DDL con psql/mysql/sqlcmd ad-hoc.\n\
             Usa i tool `project_db_create_migration` e `project_db_apply_migration`, oppure il tool di migration dello stack (Flyway, Alembic, Prisma, dotnet ef, ecc.).\n\
             Per eccezioni controllate, un admin può impostare `allow_ddl_override` sulla connessione DB del progetto.\n\
             ---\nComando: {}",
            command.chars().take(400).collect::<String>()
        );
    }

    let explicit_bg = input.get("background").and_then(Value::as_bool).unwrap_or(false);

    // ── Livello 1: parametro background esplicito dall'AI ──
    if explicit_bg {
        let routed = service::tool_run_service(ctx, input, "service").await;
        return format!(
            "[Background] Comando avviato come servizio server-side (background=true).\n{}",
            routed
        );
    }

    // ── Livello 2: lista hardcoded di comandi noti ──
    if looks_like_long_running_command(&command, &ctx.long_running_patterns) {
        let routed = service::tool_run_service(ctx, input, "service").await;
        return format!(
            "[Auto-routing] Comando long-running rilevato: avviato come servizio server-side.\n{}",
            routed
        );
    }

    // ── Livello 3: probe timeout — esegui, se non finisce in 10s ri-lancia nel terminale ──
    let work_dir = if let Some(sub) = input.get("working_dir").and_then(Value::as_str) {
        match resolve_relative_path(&ctx.root_path, sub) {
            Ok(p) => p,
            Err(e) => return format!("[Errore percorso working_dir: {}]", e.1["error"].as_str().unwrap_or("path error")),
        }
    } else {
        ctx.root_path.clone()
    };

    // M72: auto-provisioning del DB applicativo dedicato del progetto e
    // injection di NEXUS_PROJECT_DB_URL + DATABASE_URL nell'env del processo.
    // L'agente NON deve mai usare il DB 'nexus' (infrastruttura). Il DB
    // applicativo si chiama <slug>_app (con `-` → `_` per validita' Postgres).
    // Idempotente: CREATE DATABASE solo se non esiste.
    let (project_db_url, project_db_name) = ensure_project_db_url(ctx).await;

    // Bash invece di /bin/sh per garantire brace-expansion (mkdir -p a/{b,c})
    // e altre feature attese dagli agenti che generano comandi shell ricchi.
    // Fallback a /bin/sh se bash non esiste.
    let shell_path = if std::path::Path::new("/bin/bash").exists() {
        "/bin/bash"
    } else {
        "/bin/sh"
    };

    let child = Command::new(shell_path)
        .arg("-c")
        .arg(&command)
        .current_dir(&work_dir)
        .env("NEXUS_PROJECT_DB_URL", &project_db_url)
        .env("NEXUS_PROJECT_DB_NAME", &project_db_name)
        .env("DATABASE_URL", &project_db_url)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();

    let mut child = match child {
        Ok(c) => c,
        Err(e) => return format!("[Errore avvio comando '{}': {}]", command, e),
    };

    // Drain stdout/stderr IN PARALLELO con child.wait() per evitare deadlock
    // del buffer pipe Linux (~64 KB). Senza questo, comandi che producono >64KB
    // di output (es. playwright test, npm install verbose) bloccano la pipe
    // e child.wait() non ritorna mai.
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut out) = stdout_handle {
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut out, &mut buf).await;
        }
        buf
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut err) = stderr_handle {
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut err, &mut buf).await;
        }
        buf
    });

    // Probe: aspetta 10 secondi. Se finisce, ritorna output. Se no, killa e re-route.
    let probe = tokio::time::timeout(
        std::time::Duration::from_secs(RUN_COMMAND_PROBE_SECS),
        child.wait(),
    )
    .await;

    match probe {
        Ok(Ok(exit_status)) => {
            // Il processo è terminato entro il probe — leggi l'output drainato dai task paralleli
            let exit_code = exit_status.code().unwrap_or(-1);
            let stdout_bytes = stdout_task.await.unwrap_or_default();
            let stderr_bytes = stderr_task.await.unwrap_or_default();
            let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
            let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();
            // Hint semantici: per exit != 0 classifica l'errore con suggerimento diagnostico
            let hint = if exit_code != 0 {
                let diag = classify_command_error(exit_code, &stderr, &stdout);
                format!("\n\n❌ Comando fallito (exit {exit_code}). {diag}.")
            } else if exit_code == 0 && stdout.trim().is_empty() && stderr.trim().is_empty() {
                "\n[NESSUN RISULTATO: il comando è completato con successo ma non ha prodotto output. \
                 Per grep/sed questo significa che il pattern non è stato trovato o il file è vuoto. \
                 Non riprovare lo stesso comando — prova un pattern diverso o usa read_file.]".to_string()
            } else if exit_code == 1 && stdout.trim().is_empty() {
                "\n[EXIT CODE 1 + output vuoto: per grep significa nessuna corrispondenza trovata.]".to_string()
            } else {
                String::new()
            };
            // Registra risultati Playwright nella tabella jobs (fire-and-forget)
            if command.contains("playwright") {
                let summary = parse_playwright_summary(&stdout, &stderr, exit_code);
                let db = ctx.db.clone();
                let pid = ctx.project_id;
                tokio::spawn(async move {
                    let _ = sqlx::query(
                        "INSERT INTO jobs (project_id, kind, status, input) VALUES ($1, 'playwright_test', $2, $3)"
                    )
                    .bind(pid)
                    .bind(if exit_code == 0 { "passed" } else { "failed" })
                    .bind(serde_json::json!({
                        "label": summary.label,
                        "message": summary.message,
                    }))
                    .execute(&*db)
                    .await;
                });
            }

            let combined = format!(
                "EXIT CODE: {}\nSTDOUT:\n{}\nSTDERR:\n{}{}",
                exit_code, stdout, stderr, hint
            );
            if combined.chars().count() > 8000 {
                format!("{}\n[OUTPUT TRONCATO A 8000 CARATTERI]", combined.chars().take(8000).collect::<String>())
            } else {
                combined
            }
        }
        Ok(Err(e)) => {
            format!("[Errore attesa comando '{}': {}]", command, e)
        }
        Err(_) => {
            // Probe timeout: il processo non è terminato in 10s → è long-running
            // Killa il processo server-side e ri-lancia nel terminale
            let _ = child.kill().await;
            let routed = service::tool_run_service(ctx, input, "service").await;
            format!(
                "[Auto-probe] Il comando non è terminato in {}s — rilevato come long-running.\n\
                 Processo server-side terminato e ri-lanciato come servizio.\n{}",
                RUN_COMMAND_PROBE_SECS, routed
            )
        }
    }
}

// ---------------------------------------------------------------------------
// run_tests — tool dedicato per cicli test-fix-test iterativi
// ---------------------------------------------------------------------------

/// Esegue i test del progetto in modo sincrono con timeout esteso.
/// Chiamato direttamente da agent_loop.rs (non via execute_agent_tool).
pub(crate) async fn tool_run_tests(ctx: &AgentToolContext, input: &Value, test_run_number: usize) -> String {
    // 1. Determina comando test
    let explicit_cmd = input.get("command").and_then(Value::as_str);
    let filter = input.get("filter").and_then(Value::as_str);
    let command = if let Some(cmd) = explicit_cmd {
        if let Some(f) = filter {
            format!("{} {}", cmd, f)
        } else {
            cmd.to_string()
        }
    } else {
        detect_test_command(&ctx.root_path, filter)
    };

    if command.is_empty() {
        return "[Errore: impossibile rilevare il comando test per questo progetto. \
                Specifica il parametro 'command' (es. 'npm test', 'cargo test', 'pytest').]".to_string();
    }

    // 2. Working directory
    let work_dir = if let Some(sub) = input.get("working_dir").and_then(Value::as_str) {
        match resolve_relative_path(&ctx.root_path, sub) {
            Ok(p) => p,
            Err(e) => return format!("[Errore percorso working_dir: {}]", e.1["error"].as_str().unwrap_or("path error")),
        }
    } else {
        ctx.root_path.clone()
    };

    // 3. Timeout (default 120s, max 300s)
    let timeout = input.get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(RUN_TESTS_DEFAULT_TIMEOUT)
        .min(RUN_TESTS_MAX_TIMEOUT);

    // 4. Esecuzione sincrona — NESSUN auto-routing a background
    let child = Command::new("/bin/sh")
        .arg("-c")
        .arg(&command)
        .current_dir(&work_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();

    let mut child = match child {
        Ok(c) => c,
        Err(e) => return format!("[Errore avvio test '{}': {}]", command, e),
    };

    // Drain stdout/stderr in parallelo con child.wait() per evitare deadlock pipe (~64KB).
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut out) = stdout_handle {
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut out, &mut buf).await;
        }
        buf
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut err) = stderr_handle {
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut err, &mut buf).await;
        }
        buf
    });

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(timeout),
        child.wait(),
    )
    .await;

    match result {
        Ok(Ok(exit_status)) => {
            let exit_code = exit_status.code().unwrap_or(-1);
            let stdout_bytes = stdout_task.await.unwrap_or_default();
            let stderr_bytes = stderr_task.await.unwrap_or_default();
            let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
            let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();

            // Troncamento intelligente: preserva errori (in fondo)
            let truncated_stdout = smart_truncate_test_output(&stdout, 6000);
            let truncated_stderr = smart_truncate_test_output(&stderr, 2000);

            // Registra risultati Playwright nella tabella jobs
            if command.contains("playwright") {
                let summary = parse_playwright_summary(&stdout, &stderr, exit_code);
                let db = ctx.db.clone();
                let pid = ctx.project_id;
                tokio::spawn(async move {
                    let _ = sqlx::query(
                        "INSERT INTO jobs (project_id, kind, status, input) VALUES ($1, 'playwright_test', $2, $3)"
                    )
                    .bind(pid)
                    .bind(if exit_code == 0 { "passed" } else { "failed" })
                    .bind(serde_json::json!({
                        "label": summary.label,
                        "message": summary.message,
                    }))
                    .execute(&*db)
                    .await;
                });
            }

            let status_label = if exit_code == 0 { "TUTTI I TEST PASSATI" } else { "TEST FALLITI" };
            format!(
                "=== RUN TEST #{} ===\nComando: {}\nStato: {} (exit code: {})\n\n\
                 --- STDOUT ---\n{}\n\n--- STDERR ---\n{}\n=== FINE RUN TEST #{} ===",
                test_run_number, command, status_label, exit_code,
                truncated_stdout, truncated_stderr, test_run_number
            )
        }
        Ok(Err(e)) => format!("[Errore attesa test '{}': {}]", command, e),
        Err(_) => {
            let _ = child.kill().await;
            format!(
                "=== RUN TEST #{} ===\nComando: {}\n\
                 [TIMEOUT] I test non sono terminati entro {}s.\n\
                 Suggerimento: usa il parametro 'filter' per eseguire un sottoinsieme di test specifici.\n\
                 === FINE RUN TEST #{} ===",
                test_run_number, command, timeout, test_run_number
            )
        }
    }
}

/// Auto-rileva il comando test dal progetto analizzando i file di configurazione.
fn detect_test_command(root: &Path, filter: Option<&str>) -> String {
    let filter_str = filter.unwrap_or("");

    // package.json (Node.js / TypeScript)
    let pkg_json = root.join("package.json");
    if pkg_json.exists() {
        if let Ok(content) = std::fs::read_to_string(&pkg_json) {
            if let Ok(v) = serde_json::from_str::<Value>(&content) {
                if v.get("scripts").and_then(|s| s.get("test")).is_some() {
                    return if filter_str.is_empty() {
                        "npm test".to_string()
                    } else {
                        format!("npm test -- {}", filter_str)
                    };
                }
            }
        }
    }

    // Cargo.toml (Rust)
    if root.join("Cargo.toml").exists() {
        return if filter_str.is_empty() {
            "cargo test".to_string()
        } else {
            format!("cargo test {}", filter_str)
        };
    }

    // pyproject.toml / pytest.ini / setup.py (Python)
    if root.join("pyproject.toml").exists()
        || root.join("pytest.ini").exists()
        || root.join("setup.cfg").exists()
    {
        return if filter_str.is_empty() {
            "python -m pytest -v".to_string()
        } else {
            format!("python -m pytest -v -k '{}'", filter_str)
        };
    }
    if root.join("setup.py").exists() {
        return if filter_str.is_empty() {
            "python -m pytest -v".to_string()
        } else {
            format!("python -m pytest -v -k '{}'", filter_str)
        };
    }

    // *.csproj / *.sln (.NET)
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".sln") || name.ends_with(".csproj") {
                return if filter_str.is_empty() {
                    "dotnet test".to_string()
                } else {
                    format!("dotnet test --filter {}", filter_str)
                };
            }
        }
    }

    // go.mod (Go)
    if root.join("go.mod").exists() {
        return if filter_str.is_empty() {
            "go test ./...".to_string()
        } else {
            format!("go test -run {} ./...", filter_str)
        };
    }

    // Makefile con target test
    let makefile = root.join("Makefile");
    if makefile.exists() {
        if let Ok(content) = std::fs::read_to_string(&makefile) {
            if content.contains("\ntest:") || content.starts_with("test:") {
                return "make test".to_string();
            }
        }
    }

    String::new()
}

/// Troncamento intelligente per output test: 20% testa + 80% coda.
/// I sommari di errore sono tipicamente alla fine dell'output.
fn smart_truncate_test_output(output: &str, max_chars: usize) -> String {
    if output.len() <= max_chars {
        return output.to_string();
    }
    let head_size = max_chars / 5;
    let tail_size = max_chars * 4 / 5;
    let head: String = output.chars().take(head_size).collect();
    let tail: String = {
        let chars: Vec<char> = output.chars().collect();
        if chars.len() > tail_size {
            chars[chars.len() - tail_size..].iter().collect()
        } else {
            output.to_string()
        }
    };
    let omitted = output.len().saturating_sub(head_size + tail_size);
    format!(
        "{}\n\n[... {} caratteri omessi — errori e sommario preservati in fondo ...]\n\n{}",
        head, omitted, tail
    )
}

struct PlaywrightSummary {
    label: String,
    message: String,
}

fn parse_playwright_summary(stdout: &str, stderr: &str, exit_code: i32) -> PlaywrightSummary {
    let output = if stdout.is_empty() { stderr } else { stdout };
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    for line in output.lines() {
        let lower = line.to_lowercase();
        if lower.contains("passed") {
            if let Some(n) = extract_test_count(&lower, "passed") { passed = n; }
        }
        if lower.contains("failed") {
            if let Some(n) = extract_test_count(&lower, "failed") { failed = n; }
        }
        if lower.contains("skipped") {
            if let Some(n) = extract_test_count(&lower, "skipped") { skipped = n; }
        }
    }
    let total = passed + failed + skipped;
    let label = if total > 0 {
        format!("Playwright: {} test ({} ok, {} ko, {} skip)", total, passed, failed, skipped)
    } else if exit_code == 0 {
        "Playwright: test completati".to_string()
    } else {
        "Playwright: esecuzione fallita".to_string()
    };
    let message = output.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
    PlaywrightSummary { label, message }
}

fn extract_test_count(line: &str, keyword: &str) -> Option<u32> {
    let pos = line.find(keyword)?;
    let before = &line[..pos];
    before.rsplit(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// Persiste l'evento di blocco su `nexus_security_audit` (mig 0154).
/// Best-effort: se la tabella non esiste o il DB e' down, log warn e prosegue.
async fn persist_security_audit(
    ctx: &AgentToolContext,
    command: &str,
    reason: &super::safety::BlockReason,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO nexus_security_audit
           (project_id, user_id, session_id, tool_name, command_excerpt, category, message, blocked)
           VALUES ($1, $2, $3, $4, $5, $6, $7, true)"#,
    )
    .bind(ctx.project_id)
    .bind(ctx.user_id)
    .bind(ctx.session_id)
    .bind("run_command")
    .bind(command.chars().take(2000).collect::<String>())
    .bind(reason.category)
    .bind(reason.message)
    .execute(&*ctx.db)
    .await
    .map(|_| ())
}

/// M72+M74+M75 — garantisce che esista un DB applicativo dedicato per il progetto
/// e ritorna `(connection_url, db_name)`. Idempotente.
///
/// **Architettura di isolamento (Livello 6 + Livello 2):**
/// - Il DB applicativo vive in un container Postgres SEPARATO (`postgres-app`)
///   dal container infrastruttura (`postgres-nexus`). Cluster distinti: non c'e'
///   modo che l'agente raggiunga il DB Nexus anche con escalation di privilegi.
/// - L'URL ritornato usa il role `nexus_app` (NOSUPERUSER, NOCREATEROLE,
///   NOREPLICATION, NOBYPASSRLS, CREATEDB) — vedi infra/sql/init-postgres-app.sh.
///
/// Settings DB-driven (cache nei caller via sqlx pool, refresh 60s lato app):
///   - nexus_app_db_host / nexus_app_db_port (default: localhost:5434)
///   - nexus_app_db_user / nexus_app_db_password (default: nexus_app/<dev>)
///   - nexus_app_admin_user / nexus_app_admin_password (per CREATE DATABASE)
///
/// Strategia:
/// 1. Legge `projects.slug`, sanifica → nome DB `<slug>_app`
/// 2. Connessione admin (al container postgres-app, NON al nexus) per
///    CREATE DATABASE idempotente con OWNER = nexus_app
/// 3. Ritorna URL `postgresql://nexus_app:<pwd>@<host>:<port>/<db>`
///
/// Se il container postgres-app non risponde, ritorna comunque un URL valido
/// così l'env injection avviene — l'agente vedra' un errore di connessione
/// che NON contaminera' il DB Nexus.
async fn ensure_project_db_url(ctx: &AgentToolContext) -> (String, String) {
    let slug: Option<String> = sqlx::query_scalar(
        "SELECT slug FROM projects WHERE id = $1 LIMIT 1",
    )
    .bind(ctx.project_id)
    .fetch_optional(&*ctx.db)
    .await
    .ok()
    .flatten();

    let base = slug.unwrap_or_else(|| ctx.project_id.simple().to_string());
    let mut sanitized: String = base
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '_' => c,
            'A'..='Z' => c.to_ascii_lowercase(),
            _ => '_',
        })
        .collect();
    if sanitized.is_empty() {
        sanitized = ctx.project_id.simple().to_string();
    }
    if sanitized.chars().next().map_or(true, |c| c.is_ascii_digit()) {
        sanitized.insert(0, 'p');
    }
    if sanitized.len() > 56 {
        sanitized.truncate(56);
    }
    let db_name = format!("{sanitized}_app");

    // Lettura settings DB-driven (single batch, default conservativi).
    let host = load_setting_or(&ctx.db, "nexus_app_db_host", "localhost").await;
    let port = load_setting_or(&ctx.db, "nexus_app_db_port", "5434").await;
    let user = load_setting_or(&ctx.db, "nexus_app_db_user", "nexus_app").await;
    let pwd  = load_setting_or(&ctx.db, "nexus_app_db_password", "nexus_app_dev_secret").await;
    let admin_user = load_setting_or(&ctx.db, "nexus_app_admin_user", "nexus_admin").await;
    let admin_pwd  = load_setting_or(&ctx.db, "nexus_app_admin_password", "nexus_admin_secret").await;

    // CREATE DATABASE idempotente sul container postgres-app via admin role.
    let admin_url = format!("postgresql://{admin_user}:{admin_pwd}@{host}:{port}/postgres");
    match sqlx::PgPool::connect(&admin_url).await {
        Ok(admin_pool) => {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)",
            )
            .bind(&db_name)
            .fetch_one(&admin_pool)
            .await
            .unwrap_or(false);
            if !exists {
                // OWNER = nexus_app cosi' il role applicativo ha pieni poteri
                // sul SUO DB (e solo quello).
                let create_sql = format!(
                    "CREATE DATABASE \"{}\" OWNER \"{}\" TEMPLATE template0",
                    db_name, user
                );
                if let Err(e) = sqlx::query(&create_sql).execute(&admin_pool).await {
                    tracing::warn!(
                        "ensure_project_db_url: CREATE DATABASE \"{}\" fallita: {} (procedo, URL comunque iniettato)",
                        db_name, e
                    );
                } else {
                    tracing::info!(
                        "ensure_project_db_url: provisioned db=\"{}\" owner=\"{}\" project_id={}",
                        db_name, user, ctx.project_id
                    );
                }
            }
            admin_pool.close().await;
        }
        Err(e) => {
            tracing::warn!(
                "ensure_project_db_url: admin pool fallito su {}: {}. URL iniettato comunque (agente vedra' connection error, NON contaminera' nexus).",
                admin_url.replacen(&admin_pwd, "***", 1), e
            );
        }
    }

    let url = format!("postgresql://{user}:{pwd}@{host}:{port}/{db_name}");
    (url, db_name)
}

/// Helper: legge una setting da DB con fallback hardcoded conservativo.
/// Niente cache (chiamato max 6 volte per run_command, costo trascurabile vs LLM).
async fn load_setting_or(db: &sqlx::PgPool, key: &str, default: &str) -> String {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1 LIMIT 1")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| default.to_string())
}
