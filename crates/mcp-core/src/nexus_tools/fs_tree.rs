//! `utility::fs_tree` — rappresenta l'albero file del project come struttura JSON.
//!
//! Input: `{dir?, max_depth?, max_entries?}`
//! Output: albero ricorsivo `{name, type: 'file'|'dir', children?}`.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct FsTreeTool;

fn build_tree(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    entries_counter: &mut usize,
    max_entries: usize,
) -> Value {
    if depth > max_depth || *entries_counter >= max_entries {
        return json!(null);
    }
    let mut children: Vec<Value> = Vec::new();
    let mut listing: Vec<_> = match std::fs::read_dir(dir) {
        Ok(it) => it.flatten().collect(),
        Err(_) => return json!(null),
    };
    listing.sort_by_key(|e| e.file_name());
    for entry in listing {
        if *entries_counter >= max_entries {
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.')
            || name == "node_modules"
            || name == "target"
            || name == "dist"
            || name == "build"
        {
            continue;
        }
        let path = entry.path();
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        *entries_counter += 1;
        if meta.is_dir() {
            let child = build_tree(&path, depth + 1, max_depth, entries_counter, max_entries);
            children.push(json!({
                "name": name,
                "type": "dir",
                "children": child,
            }));
        } else {
            children.push(json!({
                "name": name,
                "type": "file",
                "size": meta.len(),
            }));
        }
    }
    json!(children)
}

#[async_trait]
impl NexusToolHandler for FsTreeTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let dir = args.get("dir").and_then(Value::as_str).unwrap_or("");
        let max_depth = args
            .get("max_depth")
            .and_then(Value::as_u64)
            .unwrap_or(4) as usize;
        let max_entries = args
            .get("max_entries")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(2000)
            .min(10_000);

        let full = ctx.project_root.join(dir);
        if !full.starts_with(&ctx.project_root) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }

        let mut counter = 0usize;
        let tree = build_tree(&full, 0, max_depth, &mut counter, max_entries);
        Ok(json!({
            "ok": true,
            "dir": dir,
            "entries": counter,
            "tree": tree,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "dir": {"type": "string"},
                "max_depth": {"type": "integer", "minimum": 1, "maximum": 10},
                "max_entries": {"type": "integer", "minimum": 1, "maximum": 10000}
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
    async fn test_fs_tree() {
        let tmp = std::env::temp_dir().join(format!("fst_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("a.txt"), "").unwrap();
        std::fs::write(tmp.join("sub/b.txt"), "").unwrap();
        let ctx = NexusToolContext::new(tmp.clone(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = FsTreeTool.execute(&ctx, &json!({})).await.unwrap();
        assert_eq!(out["ok"], true);
        assert!(out["entries"].as_u64().unwrap() >= 2);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
