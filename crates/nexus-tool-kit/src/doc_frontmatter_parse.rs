//! `documentation::doc_frontmatter_parse` — parsing del frontmatter YAML in un .md.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::path::Component;

pub struct DocFrontmatterParseTool;

fn parse_frontmatter(content: &str) -> Option<(Map<String, Value>, usize)> {
    let mut lines = content.lines();
    let first = lines.next()?;
    if first.trim() != "---" {
        return None;
    }
    let mut map = Map::new();
    let mut consumed_lines = 1usize;
    for line in lines {
        consumed_lines += 1;
        if line.trim() == "---" {
            return Some((map, consumed_lines));
        }
        if let Some(idx) = line.find(':') {
            let key = line[..idx].trim().to_string();
            let val = line[idx + 1..].trim().trim_matches('"').to_string();
            map.insert(key, Value::String(val));
        }
    }
    None
}

#[async_trait]
impl NexusToolHandler for DocFrontmatterParseTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("path required".into()))?;
        let pb = std::path::PathBuf::from(path);
        if pb.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }
        let full = ctx.project_root.join(&pb);
        if !full.starts_with(&ctx.project_root) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }
        let content = std::fs::read_to_string(&full).map_err(NexusToolError::Io)?;
        match parse_frontmatter(&content) {
            Some((map, _)) => Ok(
                json!({"ok": true, "path": path, "has_frontmatter": true, "fields": Value::Object(map)}),
            ),
            None => Ok(json!({"ok": true, "path": path, "has_frontmatter": false})),
        }
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
