//! `build::cargo_install_list` — `cargo install --list`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoInstallListTool;

#[async_trait]
impl NexusToolHandler for CargoInstallListTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let out = run_cmd("cargo", &["install", "--list"], &ctx.project_root, ctx.timeout_secs).await?;
        if !out.success() {
            return Err(NexusToolError::Exec { exit_code: out.exit_code, stderr: out.stderr });
        }
        let mut crates = Vec::new();
        let mut current: Option<(String, String)> = None;
        for line in out.stdout.lines() {
            if !line.starts_with(' ') && !line.is_empty() {
                // "name v1.0.0:" header line
                if let Some(stripped) = line.strip_suffix(':') {
                    let parts: Vec<&str> = stripped.splitn(2, ' ').collect();
                    if parts.len() == 2 {
                        if let Some((n, v)) = current.take() {
                            crates.push(json!({"name": n, "version": v}));
                        }
                        current = Some((parts[0].to_string(), parts[1].trim_start_matches('v').to_string()));
                    }
                }
            }
        }
        if let Some((n, v)) = current.take() {
            crates.push(json!({"name": n, "version": v}));
        }
        Ok(json!({"ok": true, "count": crates.len(), "crates": crates, "duration_ms": out.duration_ms}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only_subproc() }
}
