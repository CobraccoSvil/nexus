//! `vcs::git_archive_dry` — stima dimensione archive senza scriverlo (lista file).
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitArchiveDryTool;

#[async_trait]
impl NexusToolHandler for GitArchiveDryTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let r = args.get("ref").and_then(Value::as_str).unwrap_or("HEAD");
        let out = run_cmd("git", &["ls-tree", "-r", "--long", r], &ctx.project_root, ctx.timeout_secs).await?;
        if !out.success() {
            return Err(NexusToolError::Exec { exit_code: out.exit_code, stderr: out.stderr });
        }
        let mut total: u64 = 0;
        let mut files: u64 = 0;
        for line in out.stdout.lines() {
            // mode type sha size\tpath
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                if let Ok(sz) = parts[3].parse::<u64>() {
                    total += sz;
                    files += 1;
                }
            }
        }
        Ok(json!({"ok": true, "ref": r, "files": files, "total_bytes": total}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"ref":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only_subproc() }
}
