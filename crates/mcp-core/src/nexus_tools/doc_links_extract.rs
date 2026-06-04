//! `documentation::doc_links_extract` — estrae link Markdown `[text](url)` da un file.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Component;

pub struct DocLinksExtractTool;

fn extract_md_links(text: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            // skip image marker !
            if i > 0 && bytes[i - 1] == b'!' {
                i += 1;
                continue;
            }
            if let Some(end_text) = text[i + 1..].find(']') {
                let text_end = i + 1 + end_text;
                if text_end + 1 < bytes.len() && bytes[text_end + 1] == b'(' {
                    if let Some(end_url) = text[text_end + 2..].find(')') {
                        let url_end = text_end + 2 + end_url;
                        let label = text[i + 1..text_end].to_string();
                        let url = text[text_end + 2..url_end].to_string();
                        out.push((label, url));
                        i = url_end + 1;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    out
}

#[async_trait]
impl NexusToolHandler for DocLinksExtractTool {
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
        let links: Vec<Value> = extract_md_links(&content)
            .into_iter()
            .map(|(t, u)| json!({"text": t, "url": u}))
            .collect();
        Ok(json!({"ok": true, "path": path, "count": links.len(), "links": links}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
