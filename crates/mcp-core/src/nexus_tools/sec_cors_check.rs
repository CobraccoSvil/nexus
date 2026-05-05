//! `security::sec_cors_check` — find permissive CORS patterns.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SecCorsCheckTool;

#[async_trait]
impl NexusToolHandler for SecCorsCheckTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &[
                "Any::Any",
                "allow_origin(Any",
                "AllowOrigin::any",
                "Access-Control-Allow-Origin: *",
                "CorsLayer::permissive",
            ],
        );
        let total = counts.iter().sum::<usize>();
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "any_any": counts[0],
            "allow_origin_any": counts[1],
            "allow_origin_any_static": counts[2],
            "header_allow_all": counts[3],
            "cors_permissive": counts[4],
            "warning": total > 0,
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
