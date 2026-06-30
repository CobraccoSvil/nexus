//! `api::api_handler_count` — count handler functions and HTTP method usage.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ApiHandlerCountTool;

#[async_trait]
impl NexusToolHandler for ApiHandlerCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &[
                "get(", "post(", "put(", "delete(", "patch(", "head(", "options(",
            ],
        );
        let total = counts.iter().sum::<usize>();
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "get": counts[0],
            "post": counts[1],
            "put": counts[2],
            "delete": counts[3],
            "patch": counts[4],
            "head": counts[5],
            "options": counts[6],
            "total_methods": total,
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
