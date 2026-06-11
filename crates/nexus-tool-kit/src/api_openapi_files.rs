//! `api::api_openapi_files` — find openapi*.yaml/json files.
use super::fs_scan::walk_project_files;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ApiOpenapiFilesTool;

#[async_trait]
impl NexusToolHandler for ApiOpenapiFilesTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let found = walk_project_files(&ctx.project_root, 5, &|name| {
            let lower = name.to_lowercase();
            (lower.contains("openapi") || lower.contains("swagger"))
                && (lower.ends_with(".yaml") || lower.ends_with(".yml") || lower.ends_with(".json"))
        });
        Ok(json!({"ok": true, "count": found.len(), "files": found}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
