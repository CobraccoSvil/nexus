//! `documentation::doc_toc_extract` — estrae heading da un file Markdown.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Component;

pub struct DocTocExtractTool;

#[async_trait]
impl NexusToolHandler for DocTocExtractTool {
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
        let mut headings: Vec<Value> = vec![];
        for line in content.lines() {
            let trimmed = line.trim_start();
            let depth = trimmed.chars().take_while(|c| *c == '#').count();
            if depth >= 1 && depth <= 6 {
                let text = trimmed[depth..].trim().to_string();
                if !text.is_empty() {
                    headings.push(json!({"depth": depth, "text": text}));
                }
            }
        }
        Ok(json!({"ok": true, "path": path, "count": headings.len(), "headings": headings}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
