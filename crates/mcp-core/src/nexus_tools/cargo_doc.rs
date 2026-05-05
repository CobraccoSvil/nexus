//! `documentation::cargo_doc` — `cargo doc --no-deps --document-private-items` (no open).
//!
//! Output: `{ok, exit_code, doc_dir, generated_files, stderr_preview}`.
//! Safety: write_subproc — scrive in target/doc.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoDocTool;

#[async_trait]
impl NexusToolHandler for CargoDocTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let private_items = args
            .get("private_items")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let no_deps = args.get("no_deps").and_then(Value::as_bool).unwrap_or(true);

        let mut cmd_args: Vec<&str> = vec!["doc"];
        if no_deps {
            cmd_args.push("--no-deps");
        }
        if private_items {
            cmd_args.push("--document-private-items");
        }

        let out = run_cmd("cargo", &cmd_args, &ctx.project_root, ctx.timeout_secs.max(300)).await?;

        // Conta gli .html generati in target/doc se esiste
        let doc_dir = ctx.project_root.join("target").join("doc");
        let mut generated_files = 0usize;
        if doc_dir.exists() {
            fn count(dir: &std::path::Path, n: &mut usize, depth: usize) {
                if depth > 6 || *n >= 10000 {
                    return;
                }
                if let Ok(entries) = std::fs::read_dir(dir) {
                    for e in entries.flatten() {
                        let p = e.path();
                        if p.is_dir() {
                            count(&p, n, depth + 1);
                        } else if p.extension().and_then(|s| s.to_str()) == Some("html") {
                            *n += 1;
                        }
                    }
                }
            }
            count(&doc_dir, &mut generated_files, 0);
        }

        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "doc_dir": doc_dir.to_string_lossy(),
            "generated_files": generated_files,
            "stderr_preview": out.stderr.chars().take(2000).collect::<String>(),
            "duration_ms": out.duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "no_deps": {"type": "boolean"},
                "private_items": {"type": "boolean"}
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
    fn test_safety() {
        let s = CargoDocTool.safety();
        assert!(s.can_write_filesystem && s.can_execute_subproc);
    }
}
