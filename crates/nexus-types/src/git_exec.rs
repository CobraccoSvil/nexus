//! Punto unico esecuzione comandi git (regola L / ADR 0026).
//!
//! Estratto da mcp-core::projects (split 7.4, passo agent_tools-4) per i
//! tool agente in nexus-agent-tools (git.rs); mcp-core::projects re-esporta
//! questi simboli per i call site storici (project_git, github, indexing,
//! analyze, file_watcher).

use std::path::Path;

use tokio::process::Command;

/// Opzioni aggiuntive per l'invocazione git: `-c key=value` e variabili env
/// (usate da github.rs per credenziali effimere).
#[derive(Debug, Default, Clone)]
pub struct GitCommandOptions {
    pub configs: Vec<(String, String)>,
    pub env: Vec<(String, String)>,
}

/// Esegue `git -C <root> [configs] <args>` e ritorna `(stdout, stderr)`.
/// Errore se l'exit status non e' zero (stderr trimmata come messaggio).
pub async fn run_git_command_with_options(
    root: &Path,
    args: &[&str],
    options: &GitCommandOptions,
) -> Result<(String, String), anyhow::Error> {
    let mut command = Command::new("git");
    command.arg("-C").arg(root);

    for (key, value) in &options.configs {
        command.arg("-c").arg(format!("{key}={value}"));
    }

    command.args(args);

    for (key, value) in &options.env {
        command.env(key, value);
    }

    let output = command.output().await?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        Ok((stdout, stderr))
    } else {
        anyhow::bail!(stderr.trim().to_string())
    }
}

/// Variante senza opzioni (caso comune).
pub async fn run_git_command(
    root: &Path,
    args: &[&str],
) -> Result<(String, String), anyhow::Error> {
    run_git_command_with_options(root, args, &GitCommandOptions::default()).await
}
