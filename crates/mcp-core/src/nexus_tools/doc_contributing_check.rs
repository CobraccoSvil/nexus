//! `documentation::doc_contributing_check` — verifica CONTRIBUTING.md.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DocContributingCheckTool;

#[async_trait]
impl NexusToolHandler for DocContributingCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let candidates = [
            "CONTRIBUTING.md",
            ".github/CONTRIBUTING.md",
            "docs/CONTRIBUTING.md",
        ];
        for c in &candidates {
            let p = ctx.project_root.join(c);
            if p.is_file() {
                let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                return Ok(json!({"ok": true, "exists": true, "filename": c, "size": size}));
            }
        }
        Ok(json!({"ok": true, "exists": false}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
