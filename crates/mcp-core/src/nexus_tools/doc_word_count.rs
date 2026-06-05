//! `documentation::doc_word_count` — conta parole/righe in un file Markdown.
use super::{
    validate_no_path_traversal, NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety,
};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DocWordCountTool;

#[async_trait]
impl NexusToolHandler for DocWordCountTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("README.md");
        let full = validate_no_path_traversal(&ctx.project_root, path)?;
        let content = std::fs::read_to_string(&full).map_err(NexusToolError::Io)?;
        let lines = content.lines().count();
        let words = content.split_whitespace().count();
        let chars = content.chars().count();
        Ok(
            json!({"ok": true, "path": path, "lines": lines, "words": words, "chars": chars, "bytes": content.len()}),
        )
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
