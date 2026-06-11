//! `github::gh_workflow_list` — `gh workflow list --json ...`.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhWorkflowListTool;

#[async_trait]
impl NexusToolHandler for GhWorkflowListTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let (count, parsed, duration_ms) =
            super::run_gh_json_list(ctx, args, "workflow", 50, 200, "id,name,state,path").await?;

        Ok(json!({
            "ok": true,
            "count": count,
            "workflows": parsed,
            "duration_ms": duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "minimum": 1, "maximum": 200}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: true,
            can_write_filesystem: false,
            can_execute_subproc: true,
            network_egress: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_safety() {
        let s = GhWorkflowListTool.safety();
        assert!(s.network_egress);
        assert!(s.read_only);
    }
}
