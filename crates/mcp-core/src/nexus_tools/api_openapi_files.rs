//! `api::api_openapi_files` — find openapi*.yaml/json files.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct ApiOpenapiFilesTool;

fn walk(dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 5 { return; }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "target" || name == "node_modules" || name.starts_with('.') { continue; }
            if p.is_dir() {
                walk(&p, depth + 1, out);
            } else {
                let lower = name.to_lowercase();
                if (lower.contains("openapi") || lower.contains("swagger")) &&
                   (lower.ends_with(".yaml") || lower.ends_with(".yml") || lower.ends_with(".json")) {
                    out.push(name);
                }
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for ApiOpenapiFilesTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let mut found: Vec<String> = vec![];
        walk(&ctx.project_root, 0, &mut found);
        Ok(json!({"ok": true, "count": found.len(), "files": found}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
