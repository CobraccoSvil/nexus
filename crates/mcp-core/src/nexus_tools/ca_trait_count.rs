//! `code_analysis::ca_trait_count` — count trait declarations.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CaTraitCountTool;

#[async_trait]
impl NexusToolHandler for CaTraitCountTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &["trait ", "pub trait ", "#[async_trait]"]);
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "trait_total": counts[0],
            "pub_trait": counts[1],
            "async_trait_attr": counts[2],
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
