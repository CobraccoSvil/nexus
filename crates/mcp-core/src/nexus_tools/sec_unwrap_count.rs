//! `security::sec_unwrap_count` — count `.unwrap()` and `.expect(` (panic surface).
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SecUnwrapCountTool;

#[async_trait]
impl NexusToolHandler for SecUnwrapCountTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &[".unwrap()", ".expect(", ".unwrap_or_else", ".unwrap_or("]);
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "unwrap": counts[0],
            "expect": counts[1],
            "unwrap_or_else": counts[2],
            "unwrap_or": counts[3],
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
