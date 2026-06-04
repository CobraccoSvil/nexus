//! `deployment::deploy_env_files_count` — count .env / .env.* / .envrc files.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct DeployEnvFilesCountTool;

fn walk(dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 4 {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "target" || name == "node_modules" || name == ".git" {
                continue;
            }
            if p.is_dir() {
                walk(&p, depth + 1, out);
            } else if name == ".env" || name.starts_with(".env.") || name == ".envrc" {
                out.push(name);
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for DeployEnvFilesCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let mut found: Vec<String> = vec![];
        walk(&ctx.project_root, 0, &mut found);
        Ok(json!({"ok": true, "count": found.len(), "files": found}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
