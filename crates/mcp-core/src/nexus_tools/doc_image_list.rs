//! `documentation::doc_image_list` — lista immagini referenziate da un .md.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Component;

pub struct DocImageListTool;

fn extract_md_images(text: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'!' && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            if let Some(end_alt) = text[i + 2..].find(']') {
                let alt_end = i + 2 + end_alt;
                if alt_end + 1 < bytes.len() && bytes[alt_end + 1] == b'(' {
                    if let Some(end_url) = text[alt_end + 2..].find(')') {
                        let url_end = alt_end + 2 + end_url;
                        let alt = text[i + 2..alt_end].to_string();
                        let url = text[alt_end + 2..url_end].to_string();
                        out.push((alt, url));
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
impl NexusToolHandler for DocImageListTool {
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
        let images: Vec<Value> = extract_md_images(&content)
            .into_iter()
            .map(|(a, u)| json!({"alt": a, "url": u}))
            .collect();
        Ok(json!({"ok": true, "path": path, "count": images.len(), "images": images}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
