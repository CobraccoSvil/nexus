//! `vcs::git_count_objects` — `git count-objects -v` per repo size info.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Map, Value};

pub struct GitCountObjectsTool;

#[async_trait]
impl NexusToolHandler for GitCountObjectsTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let out = run_cmd("git", &["count-objects", "-v"], &ctx.project_root, ctx.timeout_secs).await?;
        if !out.success() {
            return Err(NexusToolError::Exec { exit_code: out.exit_code, stderr: out.stderr });
        }
        let mut map = Map::new();
        for line in out.stdout.lines() {
            if let Some((k, v)) = line.split_once(": ") {
                if let Ok(n) = v.trim().parse::<u64>() {
                    map.insert(k.to_string(), json!(n));
                } else {
                    map.insert(k.to_string(), json!(v.trim()));
                }
            }
        }
        Ok(json!({"ok": true, "stats": Value::Object(map)}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only_subproc() }
}
