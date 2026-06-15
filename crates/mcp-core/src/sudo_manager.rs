//! Client mcp-core verso il Sudo Manager Livello 1 (ADR 0017).
//!
//! Espone `execute(purpose)` che invoca `sudo /usr/local/bin/nexus-sudo-runner
//! <purpose>` come child process e ritorna stdout/stderr/exit_code. Il runner
//! valida internamente contro DB whitelist + allowlist hardcoded; mcp-core
//! NON conosce password sudo e NON puo' eseguire comandi non whitelistati.
//!
//! Setup richiesto (one-time per host):
//!   bash deploy/install-sudo-manager.sh
//!
//! Configurazione DB (regola G):
//!   - agent.sudo.manager_enabled        (default "true")
//!   - agent.sudo.runner_path            (default "/usr/local/bin/nexus-sudo-runner")
//!   - agent.sudo.audit_excerpt_max_bytes (default 4096)

use anyhow::{anyhow, Context, Result};
use sqlx::PgPool;
use tokio::process::Command;

use crate::db_settings::{read_bool, read_text};

/// Outcome di una chiamata sudo_manager::execute.
#[derive(Debug, Clone)]
pub struct SudoOutcome {
    pub purpose: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub success: bool,
}

#[derive(Debug, Clone)]
pub struct SudoConfig {
    pub enabled: bool,
    pub runner_path: String,
}

impl SudoConfig {
    pub async fn load(db: &PgPool) -> Self {
        let enabled = read_bool(db, "agent.sudo.manager_enabled", true).await;
        let runner_path = read_text(
            db,
            "agent.sudo.runner_path",
            "/usr/local/bin/nexus-sudo-runner",
        )
        .await;
        Self {
            enabled,
            runner_path,
        }
    }
}

/// Esegue un purpose whitelistato. Errore se:
///   - manager disabilitato (settings.agent.sudo.manager_enabled = false)
///   - purpose name non valida (formato kebab-case)
///   - runner binary mancante (setup non eseguito)
///   - sudo NOPASSWD non configurato (setup non eseguito)
///   - DB lookup fallisce (purpose non in whitelist)
///   - exit_code != 0 (audit salvato comunque dal runner stesso)
pub async fn execute(db: &PgPool, purpose: &str) -> Result<SudoOutcome> {
    if !is_valid_purpose_name(purpose) {
        return Err(anyhow!(
            "purpose name non valido (atteso: ^[a-z][a-z0-9-]{{2,63}}$): {purpose}"
        ));
    }
    let cfg = SudoConfig::load(db).await;
    if !cfg.enabled {
        return Err(anyhow!(
            "sudo_manager disabilitato (settings.agent.sudo.manager_enabled = false)"
        ));
    }
    if !std::path::Path::new(&cfg.runner_path).exists() {
        return Err(anyhow!(
            "binary nexus-sudo-runner non trovato in {}. Eseguire: bash deploy/install-sudo-manager.sh",
            cfg.runner_path
        ));
    }

    let started = std::time::Instant::now();
    let output = Command::new("sudo")
        .arg("--non-interactive")
        .arg(&cfg.runner_path)
        .arg(purpose)
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("spawn sudo {} {}", cfg.runner_path, purpose))?;

    let duration_ms = started.elapsed().as_millis() as u64;
    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // Detection di "sudo NOPASSWD non configurato": stderr contiene
    // "a password is required" o "sudo: a terminal is required".
    if exit_code != 0
        && (stderr.contains("a password is required") || stderr.contains("a terminal is required"))
    {
        return Err(anyhow!(
            "sudo NOPASSWD non configurato per {}. Eseguire: bash deploy/install-sudo-manager.sh",
            cfg.runner_path
        ));
    }

    let success = exit_code == 0;
    Ok(SudoOutcome {
        purpose: purpose.to_string(),
        exit_code,
        stdout,
        stderr,
        duration_ms,
        success,
    })
}

/// Variante "dry-run": verifica solo che il purpose sia registrato + abilitato,
/// senza eseguire nulla. Utile alla UI admin per disabilitare il bottone
/// "Esegui" se la pre-condizione non e' soddisfatta.
pub async fn is_executable(db: &PgPool, purpose: &str) -> Result<bool> {
    if !is_valid_purpose_name(purpose) {
        return Ok(false);
    }
    let cfg = SudoConfig::load(db).await;
    if !cfg.enabled || !std::path::Path::new(&cfg.runner_path).exists() {
        return Ok(false);
    }
    let enabled: Option<bool> =
        sqlx::query_scalar("SELECT enabled FROM nexus_sudo_purposes WHERE name = $1")
            .bind(purpose)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    Ok(enabled.unwrap_or(false))
}

fn is_valid_purpose_name(s: &str) -> bool {
    let re = regex::Regex::new(r"^[a-z][a-z0-9-]{2,63}$").expect("regex valida");
    re.is_match(s)
}

/// Diagnostica stato Sudo Manager: usata da endpoint `/api/admin/sudo/status`.
#[derive(Debug, serde::Serialize)]
pub struct SudoManagerStatus {
    pub enabled: bool,
    pub runner_installed: bool,
    pub runner_path: String,
    pub sudoers_installed: bool,
    pub purposes_count: i64,
    pub audit_recent_count: i64,
}

pub async fn status(db: &PgPool) -> Result<SudoManagerStatus> {
    let cfg = SudoConfig::load(db).await;
    let runner_installed = std::path::Path::new(&cfg.runner_path).exists();
    let sudoers_installed = std::path::Path::new("/etc/sudoers.d/nexus-runner").exists();
    let purposes_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM nexus_sudo_purposes WHERE enabled = TRUE")
            .fetch_one(db)
            .await
            .unwrap_or(0);
    let audit_recent_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM nexus_sudo_audit_log WHERE executed_at > NOW() - INTERVAL '24 hours'",
    )
    .fetch_one(db)
    .await
    .unwrap_or(0);
    Ok(SudoManagerStatus {
        enabled: cfg.enabled,
        runner_installed,
        runner_path: cfg.runner_path,
        sudoers_installed,
        purposes_count,
        audit_recent_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_purpose_names() {
        assert!(is_valid_purpose_name("playwright-install-deps"));
        assert!(is_valid_purpose_name("apt-update"));
        assert!(is_valid_purpose_name("a12"));
        assert!(!is_valid_purpose_name(""));
        assert!(!is_valid_purpose_name("ab")); // troppo corto
        assert!(!is_valid_purpose_name("Foo-Bar")); // uppercase
        assert!(!is_valid_purpose_name("foo bar")); // spazio
        assert!(!is_valid_purpose_name("foo;bar")); // shell metachar
        assert!(!is_valid_purpose_name("1foo")); // inizia con cifra
    }

    /// Limite lunghezza: 64 char totali (1 + 2..63).
    #[test]
    fn purpose_name_length_limit() {
        let ok = format!("a{}", "x".repeat(63));
        assert_eq!(ok.len(), 64);
        assert!(is_valid_purpose_name(&ok));
        let too_long = format!("a{}", "x".repeat(64));
        assert_eq!(too_long.len(), 65);
        assert!(!is_valid_purpose_name(&too_long));
    }
}
