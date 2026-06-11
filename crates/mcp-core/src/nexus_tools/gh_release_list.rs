//! `github::gh_release_list` — `gh release list --json ...`.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhReleaseListTool;

#[async_trait]
impl NexusToolHandler for GhReleaseListTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let (count, parsed, duration_ms) = super::run_gh_json_list(
            ctx,
            args,
            "release",
            20,
            100,
            "tagName,name,isDraft,isPrerelease,publishedAt,createdAt",
        )
        .await?;

        Ok(json!({
            "ok": true,
            "count": count,
            "releases": parsed,
            "duration_ms": duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "minimum": 1, "maximum": 100}
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
        assert!(GhReleaseListTool.safety().network_egress);
    }
}
