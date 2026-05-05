//! `code_analysis::cargo_edition_detect` — trova `edition` nei manifest del workspace.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

pub struct CargoEditionDetectTool;

fn walk(root: &Path, dir: &Path, by_edition: &mut HashMap<String, usize>, items: &mut Vec<Value>, depth: usize) {
    if depth > 6 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name == "target" || name.starts_with('.') {
            continue;
        }
        if p.is_dir() {
            walk(root, &p, by_edition, items, depth + 1);
            continue;
        }
        if name == "Cargo.toml" {
            if let Ok(content) = std::fs::read_to_string(&p) {
                let edition = content.lines()
                    .find(|l| l.trim_start().starts_with("edition"))
                    .and_then(|l| l.split('=').nth(1))
                    .map(|s| s.trim().trim_matches('"').to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                *by_edition.entry(edition.clone()).or_insert(0) += 1;
                let rel = p.strip_prefix(root).map(|x| x.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                items.push(json!({"manifest": rel, "edition": edition}));
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for CargoEditionDetectTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let mut by_edition = HashMap::new();
        let mut items = Vec::new();
        walk(&ctx.project_root, &ctx.project_root, &mut by_edition, &mut items, 0);
        Ok(json!({"ok": true, "manifests": items.len(), "by_edition": by_edition, "items": items}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
