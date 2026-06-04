//! `code_analysis::ca_use_count` — count `use` statements.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CaUseCountTool;

#[async_trait]
impl NexusToolHandler for CaUseCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &[
                "use ",
                "use crate::",
                "use super::",
                "use std::",
                "pub use ",
            ],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "use_total": counts[0],
            "use_crate": counts[1],
            "use_super": counts[2],
            "use_std": counts[3],
            "pub_use": counts[4],
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
