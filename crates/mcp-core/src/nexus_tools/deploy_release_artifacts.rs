//! `deployment::deploy_release_artifacts` — list common release artifact paths.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DeployReleaseArtifactsTool;

#[async_trait]
impl NexusToolHandler for DeployReleaseArtifactsTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let candidates = [
            "dist",
            "build",
            "out",
            "release",
            "artifacts",
            "target/release",
            "target/wasm32-unknown-unknown/release",
        ];
        let mut found: Vec<Value> = vec![];
        for c in &candidates {
            let p = ctx.project_root.join(c);
            if p.is_dir() {
                let mut entries = 0usize;
                let mut total = 0u64;
                if let Ok(rd) = std::fs::read_dir(&p) {
                    for entry in rd.flatten() {
                        if let Ok(meta) = entry.metadata() {
                            if meta.is_file() {
                                entries += 1;
                                total += meta.len();
                            }
                        }
                    }
                }
                found.push(json!({"path": c, "files": entries, "total_bytes": total}));
            }
        }
        Ok(json!({"ok": true, "found": found.len(), "dirs": found}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
