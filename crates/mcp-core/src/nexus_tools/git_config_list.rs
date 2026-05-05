//! `vcs::git_config_list` — `git config --list --local` enumerate config keys.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Map, Value};

pub struct GitConfigListTool;

const SENSITIVE_PREFIXES: &[&str] = &["url.", "credential.", "remote."];

#[async_trait]
impl NexusToolHandler for GitConfigListTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let out = run_cmd("git", &["config", "--list", "--local"], &ctx.project_root, ctx.timeout_secs).await?;
        if !out.success() {
            return Err(NexusToolError::Exec { exit_code: out.exit_code, stderr: out.stderr });
        }
        let mut map = Map::new();
        for line in out.stdout.lines() {
            if let Some((k, v)) = line.split_once('=') {
                let masked = SENSITIVE_PREFIXES.iter().any(|p| k.starts_with(p));
                let val = if masked && v.len() > 8 {
                    format!("{}***", &v[..4])
                } else {
                    v.to_string()
                };
                map.insert(k.to_string(), json!(val));
            }
        }
        Ok(json!({"ok": true, "count": map.len(), "config": Value::Object(map)}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only_subproc() }
}
