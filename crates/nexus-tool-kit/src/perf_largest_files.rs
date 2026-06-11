//! `performance::perf_largest_files` — top N file `.rs` per dimensione in `src/` e `crates/`.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub struct PerfLargestFilesTool;

fn collect(dir: &Path, root: &Path, depth: usize, out: &mut Vec<(String, u64)>) {
    if depth > 8 {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "target" || name == "node_modules" || name.starts_with('.') {
                continue;
            }
            if p.is_dir() {
                collect(&p, root, depth + 1, out);
            } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
                let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                if let Ok(rel) = p.strip_prefix(root) {
                    out.push((rel.to_string_lossy().replace('\\', "/"), size));
                }
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for PerfLargestFilesTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .clamp(1, 200) as usize;
        let mut all: Vec<(String, u64)> = vec![];
        let roots: [PathBuf; 2] = [
            ctx.project_root.join("src"),
            ctx.project_root.join("crates"),
        ];
        for r in &roots {
            if r.is_dir() {
                collect(r, &ctx.project_root, 0, &mut all);
            }
        }
        all.sort_by_key(|f| std::cmp::Reverse(f.1));
        let top: Vec<Value> = all
            .into_iter()
            .take(limit)
            .map(|(p, s)| json!({"path": p, "bytes": s}))
            .collect();
        Ok(json!({"ok": true, "count": top.len(), "files": top}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"limit":{"type":"integer"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
