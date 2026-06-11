//! `code_analysis::cargo_msrv_detect` — trova `rust-version` nei manifest del workspace.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct CargoMsrvDetectTool;

fn walk(root: &Path, dir: &Path, out: &mut Vec<Value>, depth: usize) {
    if depth > 6 || out.len() >= 200 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name == "target" || name.starts_with('.') {
            continue;
        }
        if p.is_dir() {
            walk(root, &p, out, depth + 1);
            continue;
        }
        if name == "Cargo.toml" {
            if let Ok(content) = std::fs::read_to_string(&p) {
                let msrv = content
                    .lines()
                    .find(|l| l.trim_start().starts_with("rust-version"))
                    .and_then(|l| l.split('=').nth(1))
                    .map(|s| s.trim().trim_matches('"').to_string());
                let rel = p
                    .strip_prefix(root)
                    .map(|x| x.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                out.push(json!({"manifest": rel, "rust_version": msrv}));
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for CargoMsrvDetectTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let mut results = Vec::new();
        walk(&ctx.project_root, &ctx.project_root, &mut results, 0);
        let with_msrv = results
            .iter()
            .filter(|v| !v["rust_version"].is_null())
            .count();
        Ok(
            json!({"ok": true, "manifests": results.len(), "with_msrv": with_msrv, "items": results}),
        )
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
