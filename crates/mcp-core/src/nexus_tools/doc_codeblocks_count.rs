//! `documentation::doc_codeblocks_count` — conta fenced code blocks per linguaggio.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Component;

pub struct DocCodeblocksCountTool;

#[async_trait]
impl NexusToolHandler for DocCodeblocksCountTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("README.md");
        let pb = std::path::PathBuf::from(path);
        if pb.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }
        let full = ctx.project_root.join(&pb);
        if !full.starts_with(&ctx.project_root) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }
        let content = std::fs::read_to_string(&full).map_err(NexusToolError::Io)?;
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut in_block = false;
        let mut total = 0usize;
        for line in content.lines() {
            if line.trim_start().starts_with("```") {
                if !in_block {
                    let lang = line
                        .trim_start()
                        .trim_start_matches("```")
                        .trim()
                        .to_string();
                    let key = if lang.is_empty() {
                        "plain".to_string()
                    } else {
                        lang
                    };
                    *counts.entry(key).or_insert(0) += 1;
                    total += 1;
                    in_block = true;
                } else {
                    in_block = false;
                }
            }
        }
        let by_lang: Vec<Value> = counts
            .into_iter()
            .map(|(k, v)| json!({"lang": k, "count": v}))
            .collect();
        Ok(json!({"ok": true, "path": path, "total": total, "by_lang": by_lang}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
