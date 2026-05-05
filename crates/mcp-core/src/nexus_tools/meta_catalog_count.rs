//! `other::meta_catalog_count` — total + implemented tool counts in NexusToolCatalog.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use crate::nexus_tool_catalog::NexusToolCatalog;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct MetaCatalogCountTool;

#[async_trait]
impl NexusToolHandler for MetaCatalogCountTool {
    async fn execute(&self, _ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        match NexusToolCatalog::global() {
            Some(cat) => Ok(json!({
                "ok": true,
                "total_specs": cat.len(),
                "implemented": cat.implemented_count(),
            })),
            None => Ok(json!({"ok": false, "error": "catalog not initialized"})),
        }
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
