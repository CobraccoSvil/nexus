//! `code_analysis::ca_derive_count` — count derive macros.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CaDeriveCountTool;

#[async_trait]
impl NexusToolHandler for CaDeriveCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &[
                "#[derive(",
                "Debug",
                "Clone",
                "Serialize",
                "Deserialize",
                "Default",
            ],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "derive_attr": counts[0],
            "debug": counts[1],
            "clone": counts[2],
            "serialize": counts[3],
            "deserialize": counts[4],
            "default": counts[5],
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
