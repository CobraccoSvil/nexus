//! `code_quality::clippy_lint` — wrapper di `cargo clippy --message-format=json`.
//!
//! Riutilizza il parser NDJSON condiviso in `parse_ndjson.rs` perché clippy
//! emette lo stesso formato di cargo check/build. La differenza è che i
//! "warning" clippy sono i veri lint che vogliamo tracciare separatamente.
//!
//! Nota: richiede che il component `clippy` sia installato
//! (`rustup component add clippy`). Se mancante, exit_code è non-zero e
//! l'errore viene riportato in `stderr`.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ClippyLintTool;

#[async_trait]
impl NexusToolHandler for ClippyLintTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let workspace_member = args
            .get("workspace_member")
            .and_then(Value::as_str)
            .map(String::from);
        let all_targets = args
            .get("all_targets")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let fix = args.get("fix").and_then(Value::as_bool).unwrap_or(false);
        let deny_warnings = args
            .get("deny_warnings")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let mut cmd: Vec<String> = vec!["clippy".into(), "--message-format=json".into()];
        if all_targets {
            cmd.push("--all-targets".into());
        }
        if fix {
            cmd.push("--fix".into());
            cmd.push("--allow-dirty".into());
            cmd.push("--allow-staged".into());
        }
        if let Some(m) = &workspace_member {
            cmd.push("-p".into());
            cmd.push(m.clone());
        }
        if deny_warnings {
            cmd.push("--".into());
            cmd.push("-D".into());
            cmd.push("warnings".into());
        }
        let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        let out = run_cmd("cargo", &refs, &ctx.project_root, ctx.timeout_secs).await?;

        let (errors, warnings) = super::parse_ndjson::extract_cargo_diagnostics(&out.stdout);
        let lints_count = errors.len() + warnings.len();

        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "errors": errors,
            "warnings": warnings,
            "lints_count": lints_count,
            "workspace_member": workspace_member,
            "fix_applied": fix,
            "deny_warnings": deny_warnings,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "workspace_member": {"type": "string"},
                "all_targets": {"type": "boolean"},
                "fix": {"type": "boolean", "description": "Applica fix automatici (--fix)"},
                "deny_warnings": {"type": "boolean", "description": "Tratta i warning come errori (-D warnings)"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        // `--fix` può modificare i sorgenti; anche senza fix, clippy scrive
        // in `target/`. Quindi write_subproc sempre.
        NexusToolSafety::write_subproc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety_write_subproc() {
        let s = ClippyLintTool.safety();
        assert!(s.can_write_filesystem);
        assert!(s.can_execute_subproc);
    }
}
