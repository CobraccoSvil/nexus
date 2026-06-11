//! `documentation::doc_toc_extract` — estrae heading da un file Markdown.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DocTocExtractTool;

#[async_trait]
impl NexusToolHandler for DocTocExtractTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let (path, content) = super::read_doc_file(ctx, args)?;
        let mut headings: Vec<Value> = vec![];
        for line in content.lines() {
            let trimmed = line.trim_start();
            let depth = trimmed.chars().take_while(|c| *c == '#').count();
            if (1..=6).contains(&depth) {
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
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
