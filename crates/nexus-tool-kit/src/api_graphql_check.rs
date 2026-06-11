//! `api::api_graphql_check` — find *.graphql / *.gql files.
use super::fs_scan::walk_project_files;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ApiGraphqlCheckTool;

#[async_trait]
impl NexusToolHandler for ApiGraphqlCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let found = walk_project_files(&ctx.project_root, 5, &|name| {
            name.ends_with(".graphql") || name.ends_with(".gql")
        });
        Ok(json!({"ok": true, "count": found.len(), "files": found}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
