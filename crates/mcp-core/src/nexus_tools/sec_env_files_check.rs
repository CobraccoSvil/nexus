//! `security::sec_env_files_check` — find .env* files and check .gitignore coverage.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct SecEnvFilesCheckTool;

fn walk(dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 4 { return; }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "target" || name == "node_modules" || name == ".git" { continue; }
            if p.is_dir() {
                walk(&p, depth + 1, out);
            } else if name.starts_with(".env") {
                out.push(name);
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for SecEnvFilesCheckTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let mut env_files: Vec<String> = vec![];
        walk(&ctx.project_root, 0, &mut env_files);
        let gi = ctx.project_root.join(".gitignore");
        let gi_content = std::fs::read_to_string(&gi).unwrap_or_default();
        let env_in_gitignore = gi_content.contains(".env");
        Ok(json!({
            "ok": true,
            "env_files": env_files,
            "count": env_files.len(),
            "gitignore_present": gi.is_file(),
            "env_in_gitignore": env_in_gitignore,
            "warning": !env_files.is_empty() && !env_in_gitignore,
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
