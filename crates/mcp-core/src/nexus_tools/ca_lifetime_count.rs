//! `code_analysis::ca_lifetime_count` — count lifetime annotations.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CaLifetimeCountTool;

#[async_trait]
impl NexusToolHandler for CaLifetimeCountTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &["'a", "'b", "'static", "for<'"]);
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "lifetime_a": counts[0],
            "lifetime_b": counts[1],
            "lifetime_static": counts[2],
            "hrtb": counts[3],
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
