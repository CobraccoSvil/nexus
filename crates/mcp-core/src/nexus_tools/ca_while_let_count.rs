//! `code_analysis::ca_while_let_count` — count loop constructs.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CaWhileLetCountTool;

#[async_trait]
impl NexusToolHandler for CaWhileLetCountTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &["while let ", "while ", "for ", "loop "]);
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "while_let": counts[0],
            "while_loop": counts[1],
            "for_loop": counts[2],
            "loop_kw": counts[3],
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
