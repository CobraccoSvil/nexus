//! `code_analysis::ca_enum_count` — count enum declarations.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CaEnumCountTool;

#[async_trait]
impl NexusToolHandler for CaEnumCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &["enum ", "pub enum ", "pub(crate) enum "],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "enum_total": counts[0],
            "pub_enum": counts[1],
            "pub_crate_enum": counts[2],
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
