//! `utility::fs_glob` — glob match minimale (`*`, `?`) ricorsivo.
//!
//! Input: `{pattern, dir?, max_results?}`
//! - `pattern`: glob su nome file (es. `*.rs`, `test_*.py`)
//! - `dir`: subdir relativa al project_root (default = root)
//! - `max_results`: cap (default 1000)
//!
//! NB: NON usa il crate `glob` (non in workspace deps). Implementa un matcher
//! semplice con `*` (qualsiasi sequenza) e `?` (un char). Il pattern si applica
//! al **nome del file**, NON al path completo.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct FsGlobTool;

fn glob_match(pattern: &str, name: &str) -> bool {
    // Matcher non-greedy ricorsivo. O(n*m) worst case ma il pattern è breve.
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = name.chars().collect();
    fn rec(p: &[char], s: &[char]) -> bool {
        if p.is_empty() {
            return s.is_empty();
        }
        match p[0] {
            '*' => {
                if p.len() == 1 {
                    return true;
                }
                for i in 0..=s.len() {
                    if rec(&p[1..], &s[i..]) {
                        return true;
                    }
                }
                false
            }
            '?' => !s.is_empty() && rec(&p[1..], &s[1..]),
            c => !s.is_empty() && s[0] == c && rec(&p[1..], &s[1..]),
        }
    }
    rec(&p, &s)
}

fn walk(
    root: &Path,
    dir: &Path,
    pattern: &str,
    out: &mut Vec<Value>,
    limit: usize,
    depth: usize,
) {
    if out.len() >= limit || depth > 10 {
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
        if meta.is_dir() {
            walk(root, &path, pattern, out, limit, depth + 1);
            continue;
        }
        if !glob_match(pattern, &name) {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| name.clone());
        out.push(json!({"path": rel, "size": meta.len()}));
    }
}

#[async_trait]
impl NexusToolHandler for FsGlobTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let pattern = args
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("pattern required".into()))?;
        let dir = args.get("dir").and_then(Value::as_str).unwrap_or("");
        let limit = args
            .get("max_results")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(1000)
            .min(10000);

        let base = ctx.project_root.join(dir);
        if !base.starts_with(&ctx.project_root) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }
        if !base.exists() {
            return Err(NexusToolError::BadInput(format!("dir not found: {}", dir)));
        }

        let mut results = Vec::new();
        walk(&ctx.project_root, &base, pattern, &mut results, limit, 0);

        Ok(json!({
            "ok": true,
            "pattern": pattern,
            "dir": dir,
            "count": results.len(),
            "truncated": results.len() >= limit,
            "items": results,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": {"type": "string"},
                "dir": {"type": "string"},
                "max_results": {"type": "integer", "minimum": 1, "maximum": 10000}
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

    #[test]
    fn test_glob_match_basic() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(glob_match("test_*.py", "test_foo.py"));
        assert!(!glob_match("*.rs", "main.py"));
        assert!(glob_match("?bc", "abc"));
        assert!(!glob_match("?bc", "abcd"));
        assert!(glob_match("*", "anything"));
    }

    #[tokio::test]
    async fn test_fs_glob_finds_rs() {
        let tmp = std::env::temp_dir().join(format!("fsg_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.rs"), "").unwrap();
        std::fs::write(tmp.join("b.rs"), "").unwrap();
        std::fs::write(tmp.join("c.txt"), "").unwrap();
        let ctx = NexusToolContext::new(tmp.clone(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = FsGlobTool
            .execute(&ctx, &json!({"pattern": "*.rs"}))
            .await
            .unwrap();
        assert_eq!(out["count"], 2);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
