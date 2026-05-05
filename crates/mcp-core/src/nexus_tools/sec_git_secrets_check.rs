//! `security::sec_git_secrets_check` — scan .git/config for credentials in URLs.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SecGitSecretsCheckTool;

#[async_trait]
impl NexusToolHandler for SecGitSecretsCheckTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let cfg = ctx.project_root.join(".git").join("config");
        if !cfg.is_file() {
            return Ok(json!({"ok": true, "exists": false}));
        }
        let content = std::fs::read_to_string(&cfg).unwrap_or_default();
        let mut suspicious: Vec<String> = vec![];
        for line in content.lines() {
            let l = line.trim();
            // Look for url = https://user:pass@host
            if l.starts_with("url = ") && l.contains('@') && l.contains("://") {
                // Mask password segment
                if let Some(idx) = l.find("://") {
                    let after = &l[idx + 3..];
                    if let Some(at) = after.find('@') {
                        if after[..at].contains(':') {
                            suspicious.push("url contains credentials".to_string());
                        }
                    }
                }
            }
        }
        Ok(json!({
            "ok": true,
            "exists": true,
            "size": content.len(),
            "issues": suspicious.len(),
            "details": suspicious,
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
