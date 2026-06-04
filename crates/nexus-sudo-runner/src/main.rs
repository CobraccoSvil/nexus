//! nexus-sudo-runner — binary privilegiato della Sudo Manager (Livello 1, ADR 0017).
//!
//! Eseguito tramite `sudo /usr/local/bin/nexus-sudo-runner <purpose-name>`
//! (NOPASSWD whitelistato in /etc/sudoers.d/nexus-runner).
//!
//! Flow:
//!   1. Argv[1] = purpose name (es. "playwright-install-deps").
//!   2. SELECT dal DB `nexus_sudo_purposes` WHERE name=$1 AND enabled=true.
//!   3. Defense-in-depth: il command_template viene riconvalidato contro un
//!      pattern allowlist HARDCODED qui (no shell metacharacters, solo
//!      programmi in PATH_ALLOWLIST). Anche se il DB e' compromesso, comandi
//!      pericolosi vengono respinti.
//!   4. Esegue con `tokio::process::Command::new(prog).args(args)`.
//!      NB: NO shell. Argomenti passati direttamente al binary -> niente
//!      injection possibile via parsing shell.
//!   5. INSERT in `nexus_sudo_audit_log` con full_command + exit_code +
//!      stdout/stderr troncati a 4KB ciascuno.
//!   6. Stampa stdout/stderr sul proprio stdout/stderr (per debug) ed esce
//!      con l'exit code del comando eseguito.
//!
//! Sicurezza:
//!   - Solo programmi in PATH_ALLOWLIST possono essere eseguiti.
//!   - Nessuna shell, nessun expansion.
//!   - Variabili d'ambiente del processo NON propagate (env-clean except PATH).
//!   - Audit log immutabile (insert-only, mai UPDATE/DELETE).

use anyhow::{anyhow, Context, Result};
use sqlx::postgres::PgPoolOptions;
use sqlx::Row;
use std::time::Instant;
use tokio::process::Command;

/// Allowlist HARDCODED dei programmi eseguibili.
/// Aggiungere qui SOLO programmi noti, mai shell o linguaggi interpretati.
/// Defense-in-depth: anche se nexus_sudo_purposes.command_template viene
/// modificato in DB con un programma diverso, viene respinto qui.
const PATH_ALLOWLIST: &[&str] = &[
    "apt-get",
    "apt",
    "dpkg",
    "systemctl",
    "service",
    "ln",
    "chmod",
    "chown",
    "mkdir",
    "rm",
];

/// Regex sicura per gli argomenti: solo `[a-zA-Z0-9._/=:@,+-]` (no spazi/shell).
/// Permette tutto cio' che serve (path, package names, target systemd) senza
/// dare via meta-caratteri. Spazi sono il separatore ARG (split lato Rust).
const ARG_SAFE_PATTERN: &str = r"^[a-zA-Z0-9._/=:@,+-]+$";

/// Limite default per stdout/stderr troncati nell'audit log.
const DEFAULT_AUDIT_EXCERPT_MAX: usize = 4096;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    // ── 1. Arg parsing ────────────────────────────────────────────────────
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: nexus-sudo-runner <purpose-name>");
        std::process::exit(64); // EX_USAGE
    }
    let purpose_name = args[1].clone();
    if !is_valid_purpose_name(&purpose_name) {
        eprintln!(
            "purpose name non valido (atteso: ^[a-z][a-z0-9-]{{2,63}}$): {}",
            purpose_name
        );
        std::process::exit(64);
    }

    // ── 2. Connessione DB ────────────────────────────────────────────────
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable".to_string()
    });
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&db_url)
        .await
        .context("connessione DB Nexus")?;

    // ── 3. Lookup purpose nel DB ─────────────────────────────────────────
    let row = sqlx::query(
        r#"
        SELECT command_template, enabled
        FROM nexus_sudo_purposes
        WHERE name = $1
        "#,
    )
    .bind(&purpose_name)
    .fetch_optional(&pool)
    .await
    .context("SELECT nexus_sudo_purposes")?;

    let row = match row {
        Some(r) => r,
        None => {
            eprintln!("purpose '{}' non registrato in nexus_sudo_purposes", purpose_name);
            std::process::exit(2);
        }
    };
    let enabled: bool = row.try_get("enabled").unwrap_or(false);
    if !enabled {
        eprintln!("purpose '{}' e' disabilitato", purpose_name);
        std::process::exit(3);
    }
    let command_template: String = row
        .try_get("command_template")
        .context("colonna command_template")?;

    // ── 4. Parsing + validazione allowlist + run ─────────────────────────
    let (program, run_args) = match parse_and_validate(&command_template) {
        Ok(v) => v,
        Err(e) => {
            // Audit del rifiuto prima di uscire (siamo gia' in contesto async)
            let _ = audit_log(
                &pool,
                &purpose_name,
                &command_template,
                None,
                "",
                &format!("REJECTED: {e}"),
                0,
            )
            .await;
            eprintln!("validazione fallita: {e}");
            std::process::exit(5);
        }
    };

    let started = Instant::now();
    let output = Command::new(&program)
        .args(&run_args)
        .env_clear()
        // Path minimo per i comandi della allowlist
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("DEBIAN_FRONTEND", "noninteractive")
        .output()
        .await
        .with_context(|| format!("spawn programma '{}'", program))?;
    let duration_ms = started.elapsed().as_millis() as i32;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);

    // ── 5. Stampa sul nostro stdout/stderr (per il caller mcp-core) ──────
    if !stdout.is_empty() {
        print!("{}", stdout);
    }
    if !stderr.is_empty() {
        eprint!("{}", stderr);
    }

    // ── 6. Audit log ────────────────────────────────────────────────────
    let _ = audit_log(
        &pool,
        &purpose_name,
        &command_template,
        Some(exit_code),
        &stdout,
        &stderr,
        duration_ms,
    )
    .await;

    std::process::exit(exit_code);
}

fn is_valid_purpose_name(s: &str) -> bool {
    let re = regex::Regex::new(r"^[a-z][a-z0-9-]{2,63}$").expect("regex valida");
    re.is_match(s)
}

/// Parsa `command_template` su spazi (no shell), valida il primo token contro
/// `PATH_ALLOWLIST`, valida ciascun arg contro `ARG_SAFE_PATTERN`.
/// Ritorna `(program, args)` pronto per `Command::new(program).args(args)`.
fn parse_and_validate(template: &str) -> Result<(String, Vec<String>)> {
    let template = template.trim();
    if template.is_empty() {
        return Err(anyhow!("command_template vuoto"));
    }
    // Rifiuta shell metacharacters (gia' bloccati anche dal CHECK SQL, ma
    // qui e' la difesa autoritativa).
    for forbidden in ['|', ';', '&', '`', '$', '<', '>', '\n', '\r', '\\'] {
        if template.contains(forbidden) {
            return Err(anyhow!(
                "metacarattere shell '{}' vietato nel command_template",
                forbidden
            ));
        }
    }

    let tokens: Vec<&str> = template.split_whitespace().collect();
    if tokens.is_empty() {
        return Err(anyhow!("command_template senza token"));
    }
    let program = tokens[0];
    if !PATH_ALLOWLIST.contains(&program) {
        return Err(anyhow!(
            "programma '{}' non in PATH_ALLOWLIST. Aggiungilo a src/main.rs e rebuilda il runner.",
            program
        ));
    }

    let arg_re = regex::Regex::new(ARG_SAFE_PATTERN).expect("regex valida");
    let args: Vec<String> = tokens[1..]
        .iter()
        .map(|t| {
            if !arg_re.is_match(t) {
                Err(anyhow!(
                    "argomento '{}' contiene caratteri non in {}",
                    t, ARG_SAFE_PATTERN
                ))
            } else {
                Ok(t.to_string())
            }
        })
        .collect::<Result<_>>()?;

    Ok((program.to_string(), args))
}

async fn audit_log(
    pool: &sqlx::PgPool,
    purpose_name: &str,
    full_command: &str,
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
    duration_ms: i32,
) -> Result<()> {
    let max_bytes: usize = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'agent.sudo.audit_excerpt_max_bytes'",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .and_then(|v| v.parse().ok())
    .unwrap_or(DEFAULT_AUDIT_EXCERPT_MAX);

    let stdout_trim = truncate_safe(stdout, max_bytes);
    let stderr_trim = truncate_safe(stderr, max_bytes);

    sqlx::query(
        r#"
        INSERT INTO nexus_sudo_audit_log
            (purpose_name, full_command, requested_by_service, exit_code,
             stdout_excerpt, stderr_excerpt, duration_ms)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(purpose_name)
    .bind(full_command)
    .bind("nexus-sudo-runner")
    .bind(exit_code)
    .bind(&stdout_trim)
    .bind(&stderr_trim)
    .bind(duration_ms)
    .execute(pool)
    .await
    .context("INSERT nexus_sudo_audit_log")?;
    Ok(())
}

fn truncate_safe(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // Tronca a char boundary
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[...troncato a {} byte...]", &s[..end], max_bytes)
}
