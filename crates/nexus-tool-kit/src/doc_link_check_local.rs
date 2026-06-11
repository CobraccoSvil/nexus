//! `documentation::doc_link_check_local` — verifica che i link locali in un .md
//! puntino a file esistenti (no http, no anchor).
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Component, PathBuf};

pub struct DocLinkCheckLocalTool;

fn extract_md_links(text: &str) -> Vec<String> {
    let mut out = vec![];
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            if let Some(end_text) = text[i + 1..].find(']') {
                let text_end = i + 1 + end_text;
                if text_end + 1 < bytes.len() && bytes[text_end + 1] == b'(' {
                    if let Some(end_url) = text[text_end + 2..].find(')') {
                        let url_end = text_end + 2 + end_url;
                        out.push(text[text_end + 2..url_end].to_string());
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
impl NexusToolHandler for DocLinkCheckLocalTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("README.md");
        let pb = PathBuf::from(path);
        if pb.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }
        let full = ctx.project_root.join(&pb);
        if !full.starts_with(&ctx.project_root) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }
        let content = std::fs::read_to_string(&full).map_err(NexusToolError::Io)?;
        let parent = full.parent().unwrap_or(&ctx.project_root).to_path_buf();
        let mut checked = 0usize;
        let mut broken: Vec<Value> = vec![];
        for url in extract_md_links(&content) {
            let trimmed = url.split('#').next().unwrap_or(&url).trim();
            if trimmed.is_empty()
                || trimmed.starts_with("http://")
                || trimmed.starts_with("https://")
                || trimmed.starts_with("mailto:")
            {
                continue;
            }
            checked += 1;
            let target_pb = PathBuf::from(trimmed);
            let resolved = if target_pb.is_absolute() {
                target_pb
            } else {
                parent.join(&target_pb)
            };
            if !resolved.exists() {
                broken.push(json!({"url": url}));
            }
        }
        Ok(json!({
            "ok": true,
            "path": path,
            "checked": checked,
            "broken_count": broken.len(),
            "broken": broken,
        }))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
