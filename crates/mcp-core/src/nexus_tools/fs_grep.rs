//! `utility::fs_grep` — cerca una regex nei file del project (mini-grep).
//!
//! Input: `{pattern, dir?, file_glob?, max_matches?, case_insensitive?}`
//! - `pattern`: regex richiesta
//! - `dir`: sub-directory (default project root)
//! - `file_glob`: regex su nome file (default match all)
//! - `max_matches`: default 200, max 2000
//!
//! Output: `{ok, count, matches: [{path, line, text}]}`

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct FsGrepTool;

fn walk_grep(
    root: &Path,
    dir: &Path,
    file_re: Option<&regex::Regex>,
    content_re: &regex::Regex,
    out: &mut Vec<Value>,
    limit: usize,
    depth: usize,
) {
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
        if name.starts_with('.')
            || name == "node_modules"
            || name == "target"
            || name == "dist"
            || name == "build"
            || name == ".git"
        {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            walk_grep(root, &path, file_re, content_re, out, limit, depth + 1);
            continue;
        }
        if let Some(re) = file_re {
            if !re.is_match(&name) {
                continue;
            }
        }
        // Skip large binaries (>2MB)
        if meta.len() > 2 * 1024 * 1024 {
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
            if content_re.is_match(line) {
                out.push(json!({
                    "path": rel,
                    "line": i + 1,
                    "text": line.chars().take(300).collect::<String>(),
                }));
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for FsGrepTool {
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
        let file_glob = args.get("file_glob").and_then(Value::as_str);
        let max_matches = args
            .get("max_matches")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(200)
            .min(2000);
        let case_insensitive = args
            .get("case_insensitive")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let full_pattern = if case_insensitive {
            format!("(?i){}", pattern)
        } else {
            pattern.to_string()
        };
        let content_re = regex::Regex::new(&full_pattern)
            .map_err(|e| NexusToolError::BadInput(format!("bad regex: {}", e)))?;
        let file_re = match file_glob {
            Some(g) => Some(
                regex::Regex::new(g)
                    .map_err(|e| NexusToolError::BadInput(format!("bad file_glob: {}", e)))?,
            ),
            None => None,
        };

        let start_dir = ctx.project_root.join(dir);
        if !start_dir.starts_with(&ctx.project_root) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }

        let mut matches = Vec::new();
        walk_grep(
            &ctx.project_root,
            &start_dir,
            file_re.as_ref(),
            &content_re,
            &mut matches,
            max_matches,
            0,
        );

        Ok(json!({
            "ok": true,
            "pattern": pattern,
            "count": matches.len(),
            "truncated": matches.len() >= max_matches,
            "matches": matches,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": {"type": "string"},
                "dir": {"type": "string"},
                "file_glob": {"type": "string"},
                "max_matches": {"type": "integer", "minimum": 1, "maximum": 2000},
                "case_insensitive": {"type": "boolean"}
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
    async fn test_fs_grep_finds() {
        let tmp = std::env::temp_dir().join(format!("fsg_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.rs"), "fn main() {\n  let hello = 1;\n}").unwrap();
        std::fs::write(tmp.join("b.rs"), "fn other() {}").unwrap();
        let ctx = NexusToolContext::new(tmp.clone(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = FsGrepTool
            .execute(&ctx, &json!({"pattern": "hello", "file_glob": "\\.rs$"}))
            .await
            .unwrap();
        assert_eq!(out["count"], 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
