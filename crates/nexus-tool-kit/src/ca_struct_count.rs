//! `code_analysis::ca_struct_count` — count struct declarations.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CaStructCountTool;

#[async_trait]
impl NexusToolHandler for CaStructCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &["struct ", "pub struct ", "pub(crate) struct "],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "struct_total": counts[0],
            "pub_struct": counts[1],
            "pub_crate_struct": counts[2],
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
