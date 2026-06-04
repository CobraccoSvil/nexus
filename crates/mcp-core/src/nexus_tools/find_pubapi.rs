//! `code_analysis::find_pubapi` — conta API pubbliche Rust per file.
//!
//! Cerca `pub fn`, `pub struct`, `pub enum`, `pub trait`, `pub mod`, `pub const`,
//! `pub type`. Restituisce conteggio totale e top-files.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

pub struct FindPubApiTool;

const KINDS: &[&str] = &[
    "pub fn",
    "pub struct",
    "pub enum",
    "pub trait",
    "pub mod",
    "pub const",
    "pub type",
    "pub static",
    "pub use",
];

fn walk(
    root: &Path,
    dir: &Path,
    by_kind: &mut HashMap<String, usize>,
    by_file: &mut HashMap<String, usize>,
    files_scanned: &mut usize,
    depth: usize,
) {
    if depth > 8 || *files_scanned > 5000 {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
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
            walk(root, &path, by_kind, by_file, files_scanned, depth + 1);
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
        *files_scanned += 1;
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or(name);
        let mut file_count = 0usize;
        for line in content.lines() {
            let trimmed = line.trim_start();
            // ignora pub(crate) e simili: matchiamo solo "pub <kind>" exact
            if trimmed.starts_with("pub(") {
                continue;
            }
            for k in KINDS {
                if trimmed.starts_with(k) {
                    *by_kind.entry((*k).to_string()).or_insert(0) += 1;
                    file_count += 1;
                    break;
                }
            }
        }
        if file_count > 0 {
            by_file.insert(rel, file_count);
        }
    }
}

#[async_trait]
impl NexusToolHandler for FindPubApiTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let top = args
            .get("top")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(20)
            .min(200);

        let mut by_kind: HashMap<String, usize> = HashMap::new();
        let mut by_file: HashMap<String, usize> = HashMap::new();
        let mut files_scanned = 0usize;
        walk(
            &ctx.project_root,
            &ctx.project_root,
            &mut by_kind,
            &mut by_file,
            &mut files_scanned,
            0,
        );

        let total: usize = by_kind.values().sum();
        let mut top_files: Vec<(String, usize)> = by_file.into_iter().collect();
        top_files.sort_by(|a, b| b.1.cmp(&a.1));
        let top_files: Vec<Value> = top_files
            .into_iter()
            .take(top)
            .map(|(p, n)| json!({"path": p, "pub_items": n}))
            .collect();

        Ok(json!({
            "ok": true,
            "total_pub_items": total,
            "files_scanned": files_scanned,
            "by_kind": by_kind,
            "top_files": top_files,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "top": {"type": "integer", "minimum": 1, "maximum": 200}
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
    async fn test_find_pubapi() {
        let tmp = std::env::temp_dir().join(format!("fp_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("lib.rs"),
            "pub fn a() {}\npub struct B;\npub(crate) fn c() {}\nfn d() {}",
        )
        .unwrap();
        let ctx = NexusToolContext::new(tmp.clone(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = FindPubApiTool.execute(&ctx, &json!({})).await.unwrap();
        assert_eq!(out["total_pub_items"], 2);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
