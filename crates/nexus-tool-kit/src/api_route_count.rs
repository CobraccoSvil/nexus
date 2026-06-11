//! `api::api_route_count` — count axum/actix/warp route definitions.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ApiRouteCountTool;

#[async_trait]
impl NexusToolHandler for ApiRouteCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &[
                ".route(",
                "Router::new",
                "axum::Router",
                "actix_web::web::resource",
                "warp::path",
                "rocket::Route",
            ],
        );
        let total = counts.iter().sum::<usize>();
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "route_method": counts[0],
            "router_new": counts[1],
            "axum_router": counts[2],
            "actix_resource": counts[3],
            "warp_path": counts[4],
            "rocket_route": counts[5],
            "total_routes": total,
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
