//! `documentation::doc_security_md_check` — verifica SECURITY.md.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DocSecurityMdCheckTool;

#[async_trait]
impl NexusToolHandler for DocSecurityMdCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let candidates = ["SECURITY.md", ".github/SECURITY.md", "docs/SECURITY.md"];
        for c in &candidates {
            let p = ctx.project_root.join(c);
            if p.is_file() {
                let content = std::fs::read_to_string(&p).unwrap_or_default();
                let lower = content.to_lowercase();
                return Ok(json!({
                    "ok": true,
                    "exists": true,
                    "filename": c,
                    "size": content.len(),
                    "has_contact": lower.contains("@") || lower.contains("contact"),
                    "has_disclosure": lower.contains("disclosure") || lower.contains("report"),
                }));
            }
        }
        Ok(json!({"ok": true, "exists": false}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
