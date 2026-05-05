//! `other::meta_health_summary` — basic health check (db + project_root + catalog).
use super::db_helper;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use crate::nexus_tool_catalog::NexusToolCatalog;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct MetaHealthSummaryTool;

#[async_trait]
impl NexusToolHandler for MetaHealthSummaryTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let project_ok = ctx.project_root.is_dir();
        let db_ok = db_helper::get_pool().await.is_ok();
        let (catalog_ok, total_tools) = match NexusToolCatalog::global() {
            Some(c) => (true, c.implemented_count()),
            None => (false, 0),
        };
        let healthy = project_ok && catalog_ok;
        Ok(json!({
            "ok": true,
            "healthy": healthy,
            "project_root_ok": project_ok,
            "db_ok": db_ok,
            "catalog_ok": catalog_ok,
            "implemented_tools": total_tools,
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
