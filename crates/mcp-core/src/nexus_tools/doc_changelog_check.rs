//! `documentation::doc_changelog_check` — verifica CHANGELOG.md e numero release.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DocChangelogCheckTool;

#[async_trait]
impl NexusToolHandler for DocChangelogCheckTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let candidates = ["CHANGELOG.md", "CHANGELOG", "HISTORY.md"];
        let mut found: Option<String> = None;
        let mut releases: usize = 0;
        for c in &candidates {
            let p = ctx.project_root.join(c);
            if p.is_file() {
                found = Some((*c).to_string());
                if let Ok(content) = std::fs::read_to_string(&p) {
                    releases = content.lines().filter(|l| l.starts_with("## ")).count();
                }
                break;
            }
        }
        Ok(json!({"ok": true, "exists": found.is_some(), "filename": found, "releases": releases}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
