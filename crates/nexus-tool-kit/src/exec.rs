//! Helper centralizzato per l'esecuzione di subprocess da dentro gli handler
//! del NexusToolCatalog.
//!
//! Centralizza:
//! - check di presenza del binario (`which` logic stile PATH lookup) →
//!   `NexusToolError::BinaryMissing` strutturato invece di panic;
//! - timeout via `tokio::time::timeout` + kill esplicito del child (evita
//!   zombie process);
//! - cattura di stdout/stderr/status in una struttura uniforme;
//! - logging strutturato tracing.
//!
//! Ogni handler può così limitarsi a 20-40 righe di parsing reale,
//! delegando la "danza del subprocess" a questo modulo.

use super::NexusToolError;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{timeout, Duration};
use tracing::{debug, warn};

use crate::sandbox;

/// Output uniforme di un subprocess lanciato tramite `run_cmd`.
#[derive(Debug, Clone)]
pub struct CmdOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
}

impl CmdOutput {
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

/// Verifica che il binario esista nel PATH. Usa `which` su Linux/macOS
/// e `where` su Windows. Ritorna `BinaryMissing` se non trovato.
///
/// Nota: il nome del binario deve essere una stringa statica per poter
/// essere passato dentro `NexusToolError::BinaryMissing`, che accetta
/// `&'static str` per evitare allocazioni.
pub async fn ensure_binary(bin: &'static str) -> Result<(), NexusToolError> {
    let lookup_cmd = if cfg!(windows) { "where" } else { "which" };
    let output = Command::new(lookup_cmd)
        .arg(bin)
        .output()
        .await
        .map_err(NexusToolError::Io)?;

    if output.status.success() {
        Ok(())
    } else {
        Err(NexusToolError::BinaryMissing(bin))
    }
}

/// Esegue un subprocess con timeout e cattura stdout/stderr.
///
/// - `bin` deve essere statico (per la diagnostica coerente con
///   `BinaryMissing`). Se non si conosce in anticipo, usare `run_cmd_owned`.
/// - `args` è una slice di argomenti già costruita dal chiamante.
/// - `cwd` è la working directory del child (tipicamente `ctx.project_root`).
/// - `timeout_secs` — se il subprocess non termina entro questo tempo, viene
///   killato con `child.kill().await` e l'errore è `Timeout`.
///
/// In caso di timeout il child è esplicitamente killato per evitare zombie.
/// La funzione NON propaga l'exit code non-zero come errore: ritorna sempre
/// `Ok(CmdOutput)` se il processo è terminato, lasciando al chiamante la
/// decisione di trattare `success() == false` come errore strutturato.
/// Questo è voluto: molti tool (`cargo check`, `cargo test`) possono avere
/// exit != 0 "legittimamente" (errori di compilazione) e l'handler vuole
/// comunque parsare l'output.
pub async fn run_cmd(
    bin: &'static str,
    args: &[&str],
    cwd: &Path,
    timeout_secs: u64,
) -> Result<CmdOutput, NexusToolError> {
    ensure_binary(bin).await?;
    run_cmd_owned(bin, args, cwd, timeout_secs).await
}

/// Come [`run_cmd`] ma con `bin` DINAMICO (non `&'static`): per quando il binario/shell
/// e' risolto a runtime — es. `sandbox::agent_shell()` ritorna Git Bash su Windows.
/// Non esegue il pre-check `ensure_binary` (che richiede `&'static` per la diagnostica
/// `BinaryMissing`): un binario assente emerge come errore IO dallo spawn.
pub async fn run_cmd_owned(
    bin: &str,
    args: &[&str],
    cwd: &Path,
    timeout_secs: u64,
) -> Result<CmdOutput, NexusToolError> {
    debug!(
        bin = bin,
        args = ?args,
        cwd = ?cwd,
        timeout = timeout_secs,
        "nexus_tools: spawning subprocess (owned bin)"
    );

    let start = std::time::Instant::now();
    // env_clear() + whitelist: il processo figlio non eredita le credenziali
    // di sistema Nexus (DATABASE_URL, REDIS_URL, ecc.) dal processo padre.
    let safe_env = sandbox::safe_env_for_direct_spawn();
    let child = crate::sandbox::isolated_command(bin)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .envs(safe_env)
        .spawn()
        .map_err(NexusToolError::Io)?;

    let wait_future = child.wait_with_output();

    match timeout(Duration::from_secs(timeout_secs), wait_future).await {
        Ok(Ok(output)) => {
            let duration_ms = start.elapsed().as_millis() as u64;
            let exit_code = output.status.code().unwrap_or(-1);
            Ok(CmdOutput {
                exit_code,
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                duration_ms,
            })
        }
        Ok(Err(e)) => Err(NexusToolError::Io(e)),
        Err(_) => {
            warn!(
                bin = bin,
                timeout = timeout_secs,
                "nexus_tools: subprocess timed out, child was already moved into wait_with_output"
            );
            Err(NexusToolError::Timeout(timeout_secs))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_ensure_binary_found() {
        // 'cargo' è sempre presente durante cargo test
        let r = ensure_binary("cargo").await;
        assert!(
            r.is_ok(),
            "cargo dovrebbe essere sempre disponibile nei test"
        );
    }

    #[tokio::test]
    async fn test_ensure_binary_missing() {
        let r = ensure_binary("definitely-not-a-real-binary-xyz-123").await;
        assert!(matches!(r, Err(NexusToolError::BinaryMissing(_))));
    }

    #[tokio::test]
    async fn test_run_cmd_success() {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        // --version è universalmente supportato da cargo
        let out = run_cmd("cargo", &["--version"], &cwd, 10).await.unwrap();
        assert!(out.success(), "cargo --version doveva avere exit 0");
        assert!(out.stdout.contains("cargo"));
        assert!(out.duration_ms < 5000);
    }

    #[tokio::test]
    async fn test_run_cmd_binary_missing() {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let r = run_cmd("definitely-not-a-real-binary-xyz-123", &[], &cwd, 5).await;
        assert!(matches!(r, Err(NexusToolError::BinaryMissing(_))));
    }
}
