//! `code_quality::cargo_fmt_check` — `cargo fmt --check` (non invasivo).
//!
//! Ritorna il numero di file che richiederebbero riformatting.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoFmtCheckTool;

#[async_trait]
impl NexusToolHandler for CargoFmtCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let out = run_cmd(
            "cargo",
            &["fmt", "--all", "--", "--check"],
            &ctx.project_root,
            ctx.timeout_secs,
        )
        .await?;

        // cargo fmt --check exit 0 = tutto ok, non-zero = diffs presenti
        // Il diff viene mandato su stdout in formato unified.
        let diff_headers: Vec<&str> = out
            .stdout
            .lines()
            .filter(|l| l.starts_with("Diff in "))
            .collect();
        let files_need_fmt = diff_headers.len();
        let clean = out.success() && files_need_fmt == 0;

        Ok(json!({
            "ok": true,
            "clean": clean,
            "exit_code": out.exit_code,
            "files_need_fmt": files_need_fmt,
            "diff_preview": out.stdout.chars().take(4000).collect::<String>(),
            "stderr_preview": out.stderr.chars().take(1000).collect::<String>(),
            "duration_ms": out.duration_ms,
        }))
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_safety() {
        assert!(CargoFmtCheckTool.safety().read_only);
        assert!(CargoFmtCheckTool.safety().can_execute_subproc);
    }
}
