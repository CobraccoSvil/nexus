//! `code_analysis::rustc_version` — `rustc --version --verbose`.
//!
//! Ritorna la versione del compilatore Rust con metadata di build (commit,
//! host triple, release channel). Utile per verificare compat toolchain
//! prima di lanciare cargo check/build.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct RustcVersionTool;

#[async_trait]
impl NexusToolHandler for RustcVersionTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let out = run_cmd(
            "rustc",
            &["--version", "--verbose"],
            &ctx.project_root,
            ctx.timeout_secs,
        )
        .await?;

        if !out.success() {
            return Err(NexusToolError::Exec {
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }

        Ok(parse_rustc_version(&out.stdout, out.duration_ms))
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}

fn parse_rustc_version(stdout: &str, duration_ms: u64) -> Value {
    let mut version = String::new();
    let mut commit_hash = String::new();
    let mut commit_date = String::new();
    let mut host = String::new();
    let mut release = String::new();
    let mut llvm = String::new();

    for line in stdout.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("rustc ") {
            version = rest.split_whitespace().next().unwrap_or("").to_string();
        } else if let Some(rest) = line.strip_prefix("commit-hash: ") {
            commit_hash = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("commit-date: ") {
            commit_date = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("host: ") {
            host = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("release: ") {
            release = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("LLVM version: ") {
            llvm = rest.to_string();
        }
    }

    json!({
        "version": version,
        "commit_hash": commit_hash,
        "commit_date": commit_date,
        "host": host,
        "release": release,
        "llvm_version": llvm,
        "duration_ms": duration_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rustc_verbose() {
        let stdout = "rustc 1.75.0 (82e1608df 2023-12-21)\nbinary: rustc\ncommit-hash: 82e1608df\ncommit-date: 2023-12-21\nhost: x86_64-pc-windows-msvc\nrelease: 1.75.0\nLLVM version: 17.0.6\n";
        let v = parse_rustc_version(stdout, 42);
        assert_eq!(v["version"], "1.75.0");
        assert_eq!(v["host"], "x86_64-pc-windows-msvc");
        assert_eq!(v["release"], "1.75.0");
        assert_eq!(v["llvm_version"], "17.0.6");
        assert_eq!(v["duration_ms"], 42);
    }

    #[test]
    fn test_safety_is_readonly() {
        let s = RustcVersionTool.safety();
        assert!(s.read_only && s.can_execute_subproc);
    }
}
