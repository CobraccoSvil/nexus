//! `build::cargo_targets_list` — lista targets via `cargo metadata`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoTargetsListTool;

#[async_trait]
impl NexusToolHandler for CargoTargetsListTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let out = run_cmd(
            "cargo",
            &["metadata", "--format-version=1", "--no-deps"],
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
        let parsed: Value = serde_json::from_str(&out.stdout).unwrap_or_else(|_| json!({}));
        let mut bins = 0usize;
        let mut libs = 0usize;
        let mut examples = 0usize;
        let mut tests = 0usize;
        let mut benches = 0usize;
        let mut targets: Vec<Value> = Vec::new();
        if let Some(pkgs) = parsed.get("packages").and_then(Value::as_array) {
            for p in pkgs {
                if let Some(ts) = p.get("targets").and_then(Value::as_array) {
                    for t in ts {
                        let kinds: Vec<String> = t
                            .get("kind")
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();
                        for k in &kinds {
                            match k.as_str() {
                                "bin" => bins += 1,
                                "lib" | "rlib" | "cdylib" | "dylib" | "staticlib" => libs += 1,
                                "example" => examples += 1,
                                "test" => tests += 1,
                                "bench" => benches += 1,
                                _ => {}
                            }
                        }
                        targets.push(json!({
                            "name": t.get("name"),
                            "kinds": kinds,
                            "package": p.get("name"),
                        }));
                    }
                }
            }
        }
        Ok(json!({
            "ok": true,
            "counts": {"bins": bins, "libs": libs, "examples": examples, "tests": tests, "benches": benches},
            "targets": targets,
            "duration_ms": out.duration_ms,
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}
