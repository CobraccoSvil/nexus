//! `vcs::git_branch_list` — wrapper read-only di `git branch -a --format=...`.
//!
//! Output: lista di branch con metadati (current, remote, upstream).

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitBranchListTool;

#[async_trait]
impl NexusToolHandler for GitBranchListTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let include_remote = args
            .get("include_remote")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let fmt = "--format=%(HEAD)%09%(refname:short)%09%(upstream:short)";
        let mut cmd_args: Vec<&str> = vec!["branch"];
        if include_remote {
            cmd_args.push("-a");
        }
        cmd_args.push(fmt);

        let out = run_cmd("git", &cmd_args, &ctx.project_root, ctx.timeout_secs).await?;
        if !out.success() {
            return Err(NexusToolError::Exec {
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }

        let mut branches: Vec<Value> = Vec::new();
        let mut current: Option<String> = None;
        for line in out.stdout.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() < 2 {
                continue;
            }
            let head_marker = parts[0];
            let name = parts[1].to_string();
            let upstream = parts.get(2).map(|s| s.to_string()).unwrap_or_default();
            let is_current = head_marker.contains('*');
            if is_current {
                current = Some(name.clone());
            }
            let is_remote = name.starts_with("remotes/") || name.contains("origin/");
            branches.push(json!({
                "name": name,
                "current": is_current,
                "remote": is_remote,
                "upstream": if upstream.is_empty() { Value::Null } else { Value::String(upstream) },
            }));
        }

        Ok(json!({
            "ok": true,
            "count": branches.len(),
            "current": current,
            "branches": branches,
            "duration_ms": out.duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "include_remote": {"type": "boolean"}
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
    fn test_safety_readonly() {
        assert!(GitBranchListTool.safety().read_only);
        assert!(GitBranchListTool.safety().can_execute_subproc);
    }
}
