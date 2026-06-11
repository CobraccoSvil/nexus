//! `build::build_log_tail` — tail target/.rustc_info.json or last log if any.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct BuildLogTailTool;

#[async_trait]
impl NexusToolHandler for BuildLogTailTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let candidates = [
            "target/.rustc_info.json",
            "target/debug/.fingerprint/.cargo-lock",
        ];
        let mut found: Vec<Value> = vec![];
        for c in &candidates {
            let p = ctx.project_root.join(c);
            if p.is_file() {
                let content = std::fs::read_to_string(&p).unwrap_or_default();
                let tail: Vec<&str> = content.lines().rev().take(10).collect();
                found.push(json!({"path": c, "tail": tail}));
            }
        }
        Ok(json!({"ok": true, "files_found": found.len(), "files": found}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
