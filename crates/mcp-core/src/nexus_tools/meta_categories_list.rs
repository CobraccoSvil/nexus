//! `other::meta_categories_list` — list all NexusToolCategory variants with counts.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use crate::nexus_tool_catalog::NexusToolCatalog;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct MetaCategoriesListTool;

#[async_trait]
impl NexusToolHandler for MetaCategoriesListTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        match NexusToolCatalog::global() {
            Some(cat) => {
                let breakdown = cat.breakdown();
                let items: Vec<Value> = breakdown
                    .iter()
                    .map(|(c, n)| json!({"category": c.name(), "count": n}))
                    .collect();
                Ok(json!({
                    "ok": true,
                    "category_count": items.len(),
                    "categories": items,
                }))
            }
            None => Ok(json!({"ok": false, "error": "catalog not initialized"})),
        }
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
