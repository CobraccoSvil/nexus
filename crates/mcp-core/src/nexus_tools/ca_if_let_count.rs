//! `code_analysis::ca_if_let_count` — count if let / let else patterns.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CaIfLetCountTool;

#[async_trait]
impl NexusToolHandler for CaIfLetCountTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &["if let ", "let Some(", "let Ok(", "else { return"]);
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "if_let": counts[0],
            "let_some": counts[1],
            "let_ok": counts[2],
            "else_return": counts[3],
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
