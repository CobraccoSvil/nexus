//! `code_analysis::ca_match_count` — count match expressions.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CaMatchCountTool;

#[async_trait]
impl NexusToolHandler for CaMatchCountTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &["match ", " => ", "_ =>"]);
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "match_kw": counts[0],
            "fat_arrow": counts[1],
            "wildcard_arm": counts[2],
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
