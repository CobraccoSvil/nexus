//! `performance::perf_unused_deps` — heuristic: dipendenze in Cargo.toml mai
//! menzionate sotto src/. Risultato è solo indicativo (no false-positive guarantee).
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct PerfUnusedDepsTool;

fn extract_deps(toml: &str) -> Vec<String> {
    let mut deps = vec![];
    let mut in_deps = false;
    for line in toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_deps = trimmed == "[dependencies]" || trimmed == "[dev-dependencies]";
            continue;
        }
        if in_deps && !trimmed.is_empty() && !trimmed.starts_with('#') {
            if let Some(eq) = trimmed.find('=') {
                deps.push(trimmed[..eq].trim().to_string());
            }
        }
    }
    deps
}

fn slurp(dir: &Path, depth: usize, into: &mut String) {
    if depth > 8 {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "target" || name.starts_with('.') {
                continue;
            }
            if p.is_dir() {
                slurp(&p, depth + 1, into);
            } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
                if let Ok(s) = std::fs::read_to_string(&p) {
                    into.push_str(&s);
                    into.push('\n');
                }
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for PerfUnusedDepsTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let cargo = ctx.project_root.join("Cargo.toml");
        if !cargo.is_file() {
            return Ok(json!({"ok": false, "error": "Cargo.toml not found"}));
        }
        let toml = std::fs::read_to_string(&cargo).map_err(NexusToolError::Io)?;
        let deps = extract_deps(&toml);
        let mut buf = String::with_capacity(64 * 1024);
        let src = ctx.project_root.join("src");
        if src.is_dir() {
            slurp(&src, 0, &mut buf);
        }
        let crates = ctx.project_root.join("crates");
        if crates.is_dir() {
            slurp(&crates, 0, &mut buf);
        }
        let unused: Vec<String> = deps
            .into_iter()
            .filter(|d| {
                let token = d.replace('-', "_");
                !buf.contains(&token)
            })
            .collect();
        Ok(
            json!({"ok": true, "candidate_unused": unused, "count": unused.len(), "note": "heuristic only"}),
        )
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
