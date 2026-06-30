//! `api::api_endpoint_list` — list literal route paths from `.route("/...")`.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct ApiEndpointListTool;

fn walk(dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 8 {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "target" || name == "node_modules" || name.starts_with('.') {
                continue;
            }
            if p.is_dir() {
                walk(&p, depth + 1, out);
            } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
                if let Ok(content) = std::fs::read_to_string(&p) {
                    for line in content.lines() {
                        if let Some(idx) = line.find(".route(\"") {
                            let after = &line[idx + 8..];
                            if let Some(end) = after.find('"') {
                                let path = &after[..end];
                                if path.starts_with('/') {
                                    out.push(path.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for ApiEndpointListTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let mut found: Vec<String> = vec![];
        for sub in ["src", "crates"] {
            let p = ctx.project_root.join(sub);
            if p.is_dir() {
                walk(&p, 0, &mut found);
            }
        }
        found.sort();
        found.dedup();
        Ok(json!({"ok": true, "count": found.len(), "endpoints": found}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
