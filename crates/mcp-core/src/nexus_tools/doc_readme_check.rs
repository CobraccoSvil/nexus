//! `documentation::doc_readme_check` — verifica esistenza README e sezioni minime.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DocReadmeCheckTool;

#[async_trait]
impl NexusToolHandler for DocReadmeCheckTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let candidates = ["README.md", "README.MD", "Readme.md", "readme.md"];
        let mut found: Option<String> = None;
        let mut content = String::new();
        for c in &candidates {
            let p = ctx.project_root.join(c);
            if p.is_file() {
                found = Some((*c).to_string());
                content = std::fs::read_to_string(&p).unwrap_or_default();
                break;
            }
        }
        let lower = content.to_lowercase();
        Ok(json!({
            "ok": true,
            "exists": found.is_some(),
            "filename": found,
            "size": content.len(),
            "has_install": lower.contains("install"),
            "has_usage": lower.contains("usage") || lower.contains("getting started"),
            "has_license": lower.contains("license"),
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
