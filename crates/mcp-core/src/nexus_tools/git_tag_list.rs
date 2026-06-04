//! `vcs::git_tag_list` — lista dei tag con optional sort by creatordate.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitTagListTool;

#[async_trait]
impl NexusToolHandler for GitTagListTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(100)
            .min(1000);

        let out = run_cmd(
            "git",
            &[
                "tag",
                "-l",
                "--sort=-creatordate",
                "--format=%(refname:short)%09%(creatordate:iso-strict)%09%(subject)",
            ],
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

        let mut tags: Vec<Value> = Vec::new();
        for line in out.stdout.lines().take(limit) {
            let parts: Vec<&str> = line.splitn(3, '\t').collect();
            if parts.is_empty() {
                continue;
            }
            tags.push(json!({
                "name": parts.first().copied().unwrap_or(""),
                "date": parts.get(1).copied().unwrap_or(""),
                "subject": parts.get(2).copied().unwrap_or(""),
            }));
        }

        Ok(json!({
            "ok": true,
            "count": tags.len(),
            "tags": tags,
            "duration_ms": out.duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "minimum": 1, "maximum": 1000}
            }
        })
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
        assert!(GitTagListTool.safety().read_only);
    }
}
