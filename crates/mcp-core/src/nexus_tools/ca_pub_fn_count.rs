//! `code_analysis::ca_pub_fn_count` — count public function declarations.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CaPubFnCountTool;

#[async_trait]
impl NexusToolHandler for CaPubFnCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &[
                "pub fn ",
                "pub async fn ",
                "pub(crate) fn ",
                "pub(super) fn ",
            ],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "pub_fn": counts[0],
            "pub_async_fn": counts[1],
            "pub_crate_fn": counts[2],
            "pub_super_fn": counts[3],
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
