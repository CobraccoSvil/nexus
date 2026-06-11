//! `utility::fs_list` — lista file in una directory del project con
//! filtro opzionale via regex sul nome.
//!
//! Input: `{dir?, pattern?, max_results?, recursive?}`
//! - `dir`: directory relativa al project_root (default: "")
//! - `pattern`: regex applicata al nome file (default: match everything)
//! - `max_results`: cap (default 500, max 5000)
//! - `recursive`: se true, walk recursivo con depth limit 10

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub struct FsListTool;

fn walk(
    root: &Path,
    base: &Path,
    re: Option<&regex::Regex>,
    recursive: bool,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<(String, u64, bool)>,
    limit: usize,
) {
    if out.len() >= limit || depth > max_depth {
        return;
    }
    let entries = match std::fs::read_dir(base) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if out.len() >= limit {
            break;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        // Skip hidden and heavy dirs
        if name.starts_with('.')
            || name == "node_modules"
            || name == "target"
            || name == "dist"
            || name == "build"
        {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_dir = meta.is_dir();
        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        let name_matches = re.map(|r| r.is_match(&name)).unwrap_or(true);
        if !is_dir && name_matches {
            out.push((rel.clone(), meta.len(), false));
        } else if is_dir && name_matches {
            out.push((rel.clone(), 0, true));
        }
        if recursive && is_dir {
            walk(root, &path, re, recursive, depth + 1, max_depth, out, limit);
        }
    }
}

#[async_trait]
impl NexusToolHandler for FsListTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let dir = args.get("dir").and_then(Value::as_str).unwrap_or("");
        let pattern = args.get("pattern").and_then(Value::as_str);
        let max_results = args
            .get("max_results")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(500)
            .min(5000);
        let recursive = args
            .get("recursive")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let full: PathBuf = ctx.project_root.join(dir);
        if !full.starts_with(&ctx.project_root) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }
        let re = match pattern {
            Some(p) => Some(
                regex::Regex::new(p)
                    .map_err(|e| NexusToolError::BadInput(format!("bad regex: {}", e)))?,
            ),
            None => None,
        };

        let mut results: Vec<(String, u64, bool)> = Vec::new();
        walk(
            &ctx.project_root,
            &full,
            re.as_ref(),
            recursive,
            0,
            10,
            &mut results,
            max_results,
        );

        let out: Vec<Value> = results
            .into_iter()
            .map(|(path, size, is_dir)| json!({"path": path, "size": size, "is_dir": is_dir}))
            .collect();

        Ok(json!({
            "ok": true,
            "dir": dir,
            "count": out.len(),
            "entries": out,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "dir": {"type": "string", "description": "Relative directory (default '')"},
                "pattern": {"type": "string", "description": "Regex on file name"},
                "max_results": {"type": "integer", "minimum": 1, "maximum": 5000},
                "recursive": {"type": "boolean"}
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
    async fn test_fs_list_basic() {
        let tmp = std::env::temp_dir().join(format!("fsl_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.rs"), "").unwrap();
        std::fs::write(tmp.join("b.rs"), "").unwrap();
        std::fs::write(tmp.join("c.txt"), "").unwrap();
        let ctx = NexusToolContext::new(tmp.clone(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = FsListTool
            .execute(&ctx, &json!({"pattern": "\\.rs$"}))
            .await
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["count"], 2);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
