//! `deployment::deploy_systemd_check` — find *.service / *.socket / *.timer units.
use super::fs_scan::walk_project_files;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DeploySystemdCheckTool;

#[async_trait]
impl NexusToolHandler for DeploySystemdCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let found = walk_project_files(&ctx.project_root, 5, &|name| {
            name.ends_with(".service") || name.ends_with(".socket") || name.ends_with(".timer")
        });
        Ok(json!({"ok": true, "count": found.len(), "units": found}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
