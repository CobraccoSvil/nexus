//! `documentation::doc_word_count` — conta parole/righe in un file Markdown.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Component;

pub struct DocWordCountTool;

#[async_trait]
impl NexusToolHandler for DocWordCountTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let path = args.get("path").and_then(Value::as_str).unwrap_or("README.md");
        let pb = std::path::PathBuf::from(path);
        if pb.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }
        let full = ctx.project_root.join(&pb);
        if !full.starts_with(&ctx.project_root) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }
        let content = std::fs::read_to_string(&full).map_err(NexusToolError::Io)?;
        let lines = content.lines().count();
        let words = content.split_whitespace().count();
        let chars = content.chars().count();
        Ok(json!({"ok": true, "path": path, "lines": lines, "words": words, "chars": chars, "bytes": content.len()}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
