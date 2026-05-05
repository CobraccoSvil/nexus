//! `build::build_incremental_dir` — check for incremental compilation directory.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct BuildIncrementalDirTool;

#[async_trait]
impl NexusToolHandler for BuildIncrementalDirTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let mut found: Vec<Value> = vec![];
        let candidates = [
            "target/debug/incremental",
            "target/release/incremental",
        ];
        for c in &candidates {
            let p = ctx.project_root.join(c);
            if p.is_dir() {
                let mut entries = 0usize;
                if let Ok(rd) = std::fs::read_dir(&p) {
                    for _ in rd.flatten() { entries += 1; }
                }
                found.push(json!({"path": c, "entries": entries}));
            }
        }
        Ok(json!({"ok": true, "incremental_dirs": found}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
