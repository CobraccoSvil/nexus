//! `code_analysis::ca_mod_count` — count module declarations.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CaModCountTool;

#[async_trait]
impl NexusToolHandler for CaModCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &["mod ", "pub mod ", "pub(crate) mod ", "#[cfg(test)]"],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "mod_total": counts[0],
            "pub_mod": counts[1],
            "pub_crate_mod": counts[2],
            "cfg_test": counts[3],
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
