//! `code_analysis::ca_doc_comment_count` — count doc comments (///, //!).
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CaDocCommentCountTool;

#[async_trait]
impl NexusToolHandler for CaDocCommentCountTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &["/// ", "//! ", "/** ", "*/"]);
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "outer_doc": counts[0],
            "inner_doc": counts[1],
            "block_doc": counts[2],
            "block_close": counts[3],
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
