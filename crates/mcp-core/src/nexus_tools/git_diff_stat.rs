//! `vcs::git_diff_stat` — `git diff --shortstat <range>` summary.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitDiffStatTool;

#[async_trait]
impl NexusToolHandler for GitDiffStatTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let range = args.get("range").and_then(Value::as_str).unwrap_or("HEAD");
        let out = run_cmd("git", &["diff", "--shortstat", range], &ctx.project_root, ctx.timeout_secs).await?;
        if !out.success() {
            return Err(NexusToolError::Exec { exit_code: out.exit_code, stderr: out.stderr });
        }
        // " 3 files changed, 12 insertions(+), 4 deletions(-)"
        let line = out.stdout.trim();
        let files = line.split_whitespace().next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        let mut ins = 0u64;
        let mut del = 0u64;
        for tok in line.split(',') {
            let t = tok.trim();
            if t.contains("insertion") {
                ins = t.split_whitespace().next().and_then(|s| s.parse().ok()).unwrap_or(0);
            } else if t.contains("deletion") {
                del = t.split_whitespace().next().and_then(|s| s.parse().ok()).unwrap_or(0);
            }
        }
        Ok(json!({"ok": true, "range": range, "files_changed": files, "insertions": ins, "deletions": del, "raw": line}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"range":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only_subproc() }
}
