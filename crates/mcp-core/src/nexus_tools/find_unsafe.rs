//! `code_analysis::find_unsafe` — trova `unsafe` blocks/fn nei file Rust.
//!
//! Output: `{count, files: [{path, line, kind, snippet}]}`.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct FindUnsafeTool;

fn walk_unsafe(root: &Path, dir: &Path, out: &mut Vec<Value>, limit: usize, depth: usize) {
    if out.len() >= limit || depth > 8 {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if out.len() >= limit {
            break;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            walk_unsafe(root, &path, out, limit, depth + 1);
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        if meta.len() > 2_000_000 {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| name.clone());
        for (i, line) in content.lines().enumerate() {
            if out.len() >= limit {
                break;
            }
            // matching grezzo: cerca "unsafe" come parola
            let trimmed = line.trim_start();
            if trimmed.contains("unsafe ") || trimmed == "unsafe" || trimmed.starts_with("unsafe{")
            {
                let kind = if trimmed.contains("unsafe fn") {
                    "fn"
                } else if trimmed.contains("unsafe trait") {
                    "trait"
                } else if trimmed.contains("unsafe impl") {
                    "impl"
                } else {
                    "block"
                };
                out.push(json!({
                    "path": rel,
                    "line": i + 1,
                    "kind": kind,
                    "snippet": trimmed.chars().take(200).collect::<String>(),
                }));
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for FindUnsafeTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let limit = args
            .get("max_results")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(500)
            .min(5000);

        let mut results = Vec::new();
        walk_unsafe(&ctx.project_root, &ctx.project_root, &mut results, limit, 0);

        // count by kind
        let mut by_kind: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for r in &results {
            if let Some(k) = r.get("kind").and_then(Value::as_str) {
                *by_kind.entry(k.to_string()).or_insert(0) += 1;
            }
        }

        Ok(json!({
            "ok": true,
            "count": results.len(),
            "truncated": results.len() >= limit,
            "by_kind": by_kind,
            "items": results,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "max_results": {"type": "integer", "minimum": 1, "maximum": 5000}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_find_unsafe() {
        let tmp = std::env::temp_dir().join(format!("fu_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("a.rs"),
            "fn safe() {}\nunsafe fn bad() {}\nfn x() { unsafe { *p = 1; } }",
        )
        .unwrap();
        let ctx = NexusToolContext::new(tmp.clone(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = FindUnsafeTool.execute(&ctx, &json!({})).await.unwrap();
        assert!(out["count"].as_u64().unwrap() >= 2);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
