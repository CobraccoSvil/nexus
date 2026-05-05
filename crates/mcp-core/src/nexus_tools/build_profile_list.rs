//! `build::build_profile_list` — list `[profile.*]` sections in root Cargo.toml.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct BuildProfileListTool;

#[async_trait]
impl NexusToolHandler for BuildProfileListTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let cargo = ctx.project_root.join("Cargo.toml");
        if !cargo.is_file() {
            return Ok(json!({"ok": true, "exists": false, "profiles": []}));
        }
        let content = std::fs::read_to_string(&cargo).unwrap_or_default();
        let mut profiles: Vec<String> = vec![];
        for line in content.lines() {
            let l = line.trim();
            if let Some(rest) = l.strip_prefix("[profile.") {
                if let Some(end) = rest.find(']') {
                    profiles.push(rest[..end].to_string());
                }
            }
        }
        Ok(json!({"ok": true, "exists": true, "profiles": profiles, "count": profiles.len()}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
