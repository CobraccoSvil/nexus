//! `code_analysis::ca_fn_count` — count fn declarations.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CaFnCountTool;

#[async_trait]
impl NexusToolHandler for CaFnCountTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &["fn ", "async fn ", "const fn ", "unsafe fn "]);
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "fn_total": counts[0],
            "async_fn": counts[1],
            "const_fn": counts[2],
            "unsafe_fn": counts[3],
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
