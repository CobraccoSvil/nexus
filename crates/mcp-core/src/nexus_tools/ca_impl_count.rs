//! `code_analysis::ca_impl_count` — count impl blocks.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CaImplCountTool;

#[async_trait]
impl NexusToolHandler for CaImplCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &["impl ", "impl<", "impl Drop", "impl Default"],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "impl_total": counts[0],
            "impl_generic": counts[1],
            "impl_drop": counts[2],
            "impl_default": counts[3],
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
