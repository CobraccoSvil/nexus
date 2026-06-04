//! `api::api_grpc_check` — find *.proto files.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct ApiGrpcCheckTool;

fn walk(dir: &Path, depth: usize, out: &mut Vec<Value>) {
    if depth > 6 {
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
            } else if name.ends_with(".proto") {
                let content = std::fs::read_to_string(&p).unwrap_or_default();
                let services = content.matches("service ").count();
                let messages = content.matches("message ").count();
                let rpcs = content.matches("rpc ").count();
                out.push(
                    json!({"name": name, "services": services, "messages": messages, "rpcs": rpcs}),
                );
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for ApiGrpcCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let mut found: Vec<Value> = vec![];
        walk(&ctx.project_root, 0, &mut found);
        Ok(json!({"ok": true, "count": found.len(), "protos": found}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
