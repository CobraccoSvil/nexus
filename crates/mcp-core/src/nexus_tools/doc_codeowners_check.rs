//! `documentation::doc_codeowners_check` — verifica CODEOWNERS file.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DocCodeownersCheckTool;

#[async_trait]
impl NexusToolHandler for DocCodeownersCheckTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let candidates = [".github/CODEOWNERS", "CODEOWNERS", "docs/CODEOWNERS"];
        for c in &candidates {
            let p = ctx.project_root.join(c);
            if p.is_file() {
                let content = std::fs::read_to_string(&p).unwrap_or_default();
                let entries = content.lines().filter(|l| !l.trim().is_empty() && !l.trim().starts_with('#')).count();
                return Ok(json!({"ok": true, "exists": true, "filename": c, "entries": entries}));
            }
        }
        Ok(json!({"ok": true, "exists": false}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
