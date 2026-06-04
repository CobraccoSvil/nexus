//! `code_analysis::find_todos` — cerca `TODO`/`FIXME`/`HACK`/`XXX` nei sorgenti.
//!
//! Output: lista di `{path, line, marker, text}`.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct FindTodosTool;

const DEFAULT_MARKERS: &[&str] = &["TODO", "FIXME", "HACK", "XXX", "BUG", "NOTE"];

fn is_source_ext(ext: &str) -> bool {
    matches!(
        ext,
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "py"
            | "go"
            | "java"
            | "kt"
            | "rb"
            | "c"
            | "cpp"
            | "cc"
            | "cxx"
            | "hpp"
            | "h"
            | "cs"
            | "php"
            | "swift"
            | "sh"
            | "bash"
            | "sql"
            | "md"
            | "yaml"
            | "yml"
            | "toml"
    )
}

fn walk_todos(
    root: &Path,
    dir: &Path,
    markers_re: &regex::Regex,
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
        {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            walk_todos(root, &path, markers_re, out, limit, depth + 1);
            continue;
        }
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        if !is_source_ext(&ext) {
            continue;
        }
        if meta.len() > 1_000_000 {
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
            if let Some(m) = markers_re.find(line) {
                out.push(json!({
                    "path": rel,
                    "line": i + 1,
                    "marker": m.as_str(),
                    "text": line.trim().chars().take(200).collect::<String>(),
                }));
            }
        }
    }
}

#[async_trait]
impl NexusToolHandler for FindTodosTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let custom_markers: Vec<String> = args
            .get("markers")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let markers_owned: Vec<String> = if custom_markers.is_empty() {
            DEFAULT_MARKERS.iter().map(|s| s.to_string()).collect()
        } else {
            custom_markers
        };

        let pattern = format!("\\b({})\\b", markers_owned.join("|"));
        let re = regex::Regex::new(&pattern)
            .map_err(|e| NexusToolError::BadInput(format!("bad marker regex: {}", e)))?;
        let limit = args
            .get("max_results")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(500)
            .min(5000);

        let mut results = Vec::new();
        walk_todos(
            &ctx.project_root,
            &ctx.project_root,
            &re,
            &mut results,
            limit,
            0,
        );

        // Group counts per marker
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for r in &results {
            if let Some(m) = r.get("marker").and_then(Value::as_str) {
                *counts.entry(m.to_string()).or_insert(0) += 1;
            }
        }

        Ok(json!({
            "ok": true,
            "count": results.len(),
            "truncated": results.len() >= limit,
            "by_marker": counts,
            "items": results,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "markers": {"type": "array", "items": {"type": "string"}},
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
    async fn test_find_todos() {
        let tmp = std::env::temp_dir().join(format!("ftd_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("x.rs"),
            "fn main() {\n  // TODO: implement\n  // FIXME broken\n}",
        )
        .unwrap();
        let ctx = NexusToolContext::new(tmp.clone(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = FindTodosTool.execute(&ctx, &json!({})).await.unwrap();
        assert_eq!(out["count"], 2);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
