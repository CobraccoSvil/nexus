//! `documentation::doc_md_lint` — lint markdown base (long lines, trailing space, tabs).
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Component;

pub struct DocMdLintTool;

#[async_trait]
impl NexusToolHandler for DocMdLintTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let path = args.get("path").and_then(Value::as_str).unwrap_or("README.md");
        let max_len = args.get("max_line").and_then(Value::as_i64).unwrap_or(120) as usize;
        let pb = std::path::PathBuf::from(path);
        if pb.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }
        let full = ctx.project_root.join(&pb);
        if !full.starts_with(&ctx.project_root) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }
        let content = std::fs::read_to_string(&full).map_err(NexusToolError::Io)?;
        let mut findings: Vec<Value> = vec![];
        for (i, line) in content.lines().enumerate() {
            let lineno = i + 1;
            if line.len() > max_len {
                findings.push(json!({"line": lineno, "rule": "line_too_long", "len": line.len()}));
            }
            if line.ends_with(' ') || line.ends_with('\t') {
                findings.push(json!({"line": lineno, "rule": "trailing_whitespace"}));
            }
            if line.contains('\t') {
                findings.push(json!({"line": lineno, "rule": "tab_indent"}));
            }
        }
        Ok(json!({"ok": true, "path": path, "issues": findings.len(), "findings": findings}))
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string"},"max_line":{"type":"integer"}}})
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
