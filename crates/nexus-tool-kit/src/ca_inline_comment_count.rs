//! `code_analysis::ca_inline_comment_count` — count inline comments.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CaInlineCommentCountTool;

#[async_trait]
impl NexusToolHandler for CaInlineCommentCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &["// ", "/* "]);
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "line_comment": counts[0],
            "block_comment": counts[1],
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
