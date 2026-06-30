//! `performance::perf_loc_per_crate` — LOC per crate in workspace `crates/*/src`.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct PerfLocPerCrateTool;

fn count_loc(dir: &Path, depth: usize) -> usize {
    if depth > 6 {
        return 0;
    }
    let mut total = 0usize;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += count_loc(&p, depth + 1);
            } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
                if let Ok(s) = std::fs::read_to_string(&p) {
                    total += s.lines().count();
                }
            }
        }
    }
    total
}

#[async_trait]
impl NexusToolHandler for PerfLocPerCrateTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let crates_dir = ctx.project_root.join("crates");
        let mut out: Vec<Value> = vec![];
        if crates_dir.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&crates_dir) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    if !p.is_dir() {
                        continue;
                    }
                    let src = p.join("src");
                    let loc = if src.is_dir() { count_loc(&src, 0) } else { 0 };
                    out.push(json!({"crate": entry.file_name().to_string_lossy(), "loc": loc}));
                }
            }
        } else {
            let src = ctx.project_root.join("src");
            if src.is_dir() {
                out.push(json!({"crate": "(root)", "loc": count_loc(&src, 0)}));
            }
        }
        out.sort_by(|a, b| {
            b["loc"]
                .as_u64()
                .unwrap_or(0)
                .cmp(&a["loc"].as_u64().unwrap_or(0))
        });
        Ok(json!({"ok": true, "count": out.len(), "crates": out}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
