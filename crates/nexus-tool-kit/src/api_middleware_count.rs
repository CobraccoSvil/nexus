//! `api::api_middleware_count` — count middleware patterns.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ApiMiddlewareCountTool;

#[async_trait]
impl NexusToolHandler for ApiMiddlewareCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &[
                ".layer(",
                "tower::ServiceBuilder",
                "from_fn(",
                "TraceLayer",
                "CorsLayer",
                "AuthLayer",
            ],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "layer_call": counts[0],
            "service_builder": counts[1],
            "from_fn": counts[2],
            "trace_layer": counts[3],
            "cors_layer": counts[4],
            "auth_layer": counts[5],
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
