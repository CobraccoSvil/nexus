//! `code_analysis::find_todos` — cerca `TODO`/`FIXME`/`HACK`/`XXX` nei sorgenti.
//!
//! Output: lista di `{path, line, marker, text}`.

use super::fs_scan::scan_file_lines;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

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

        let results = scan_file_lines(
            &ctx.project_root,
            &ctx.project_root,
            1_000_000,
            limit,
            &|_name, path| {
                let ext = path
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                is_source_ext(&ext)
            },
            &mut |rel, line_no, line| {
                re.find(line).map(|m| {
                    json!({
                        "path": rel,
                        "line": line_no,
                        "marker": m.as_str(),
                        "text": line.trim().chars().take(200).collect::<String>(),
                    })
                })
            },
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
