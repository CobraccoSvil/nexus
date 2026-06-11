//! `documentation::doc_codeblocks_extract` — estrae fenced code blocks da un .md.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DocCodeblocksExtractTool;

#[async_trait]
impl NexusToolHandler for DocCodeblocksExtractTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let (path, content) = super::read_doc_file(ctx, args)?;
        let mut blocks: Vec<Value> = vec![];
        let mut in_block = false;
        let mut current_lang = String::new();
        let mut current_code: Vec<String> = vec![];
        let mut start_line = 0usize;
        for (i, line) in content.lines().enumerate() {
            if line.trim_start().starts_with("```") {
                if !in_block {
                    in_block = true;
                    current_lang = line
                        .trim_start()
                        .trim_start_matches("```")
                        .trim()
                        .to_string();
                    current_code.clear();
                    start_line = i + 1;
                } else {
                    blocks.push(json!({
                        "lang": current_lang,
                        "start_line": start_line,
                        "lines": current_code.len(),
                        "code": current_code.join("\n"),
                    }));
                    in_block = false;
                }
            } else if in_block {
                current_code.push(line.to_string());
            }
        }
        Ok(json!({"ok": true, "path": path, "count": blocks.len(), "blocks": blocks}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
