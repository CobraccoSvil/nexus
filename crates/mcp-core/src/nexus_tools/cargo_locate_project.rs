//! `build::cargo_locate_project` — `cargo locate-project` (path al Cargo.toml).
//!
//! Output: `{root_manifest, workspace_manifest?, is_workspace}`.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoLocateProjectTool;

#[async_trait]
impl NexusToolHandler for CargoLocateProjectTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let out_root = run_cmd(
            "cargo",
            &["locate-project", "--message-format=json"],
            &ctx.project_root,
            ctx.timeout_secs,
        )
        .await?;

        if !out_root.success() {
            return Err(NexusToolError::Exec {
                exit_code: out_root.exit_code,
                stderr: out_root.stderr,
            });
        }

        let root_parsed: Value =
            serde_json::from_str(out_root.stdout.trim()).unwrap_or_else(|_| json!({}));
        let root_manifest = root_parsed
            .get("root")
            .and_then(Value::as_str)
            .map(|s| s.to_string());

        let out_ws = run_cmd(
            "cargo",
            &["locate-project", "--workspace", "--message-format=json"],
            &ctx.project_root,
            ctx.timeout_secs,
        )
        .await?;
        let workspace_manifest = if out_ws.success() {
            let parsed: Value =
                serde_json::from_str(out_ws.stdout.trim()).unwrap_or_else(|_| json!({}));
            parsed
                .get("root")
                .and_then(Value::as_str)
                .map(|s| s.to_string())
        } else {
            None
        };

        let is_workspace = workspace_manifest
            .as_ref()
            .zip(root_manifest.as_ref())
            .map(|(ws, root)| ws != root)
            .unwrap_or(false);

        Ok(json!({
            "ok": true,
            "root_manifest": root_manifest,
            "workspace_manifest": workspace_manifest,
            "is_workspace": is_workspace,
            "duration_ms": out_root.duration_ms + out_ws.duration_ms,
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
        assert!(CargoLocateProjectTool.safety().read_only);
    }
}
