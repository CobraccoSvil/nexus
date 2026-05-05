//! `code_analysis::ca_todo_fixme_count` — count TODO/FIXME/XXX/HACK markers.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CaTodoFixmeCountTool;

#[async_trait]
impl NexusToolHandler for CaTodoFixmeCountTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &["TODO", "FIXME", "XXX", "HACK", "NOTE"]);
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "todo": counts[0],
            "fixme": counts[1],
            "xxx": counts[2],
            "hack": counts[3],
            "note": counts[4],
            "total": counts.iter().sum::<usize>(),
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
