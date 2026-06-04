//! `code_analysis::ca_macro_count` — count macro definitions.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CaMacroCountTool;

#[async_trait]
impl NexusToolHandler for CaMacroCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &[
                "macro_rules!",
                "#[proc_macro]",
                "#[proc_macro_derive",
                "#[proc_macro_attribute]",
            ],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "macro_rules": counts[0],
            "proc_macro": counts[1],
            "proc_macro_derive": counts[2],
            "proc_macro_attribute": counts[3],
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
