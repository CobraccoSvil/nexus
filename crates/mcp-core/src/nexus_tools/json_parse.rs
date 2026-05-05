//! `utility::json_parse` — valida e pretty-print una stringa JSON.
//!
//! Input: `{content}` oppure `{path}` relativo al project.
//! Output: `{ok, valid, type, size, pretty?, error?}`.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct JsonParseTool;

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[async_trait]
impl NexusToolHandler for JsonParseTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let content = if let Some(c) = args.get("content").and_then(Value::as_str) {
            c.to_string()
        } else if let Some(path) = args.get("path").and_then(Value::as_str) {
            let full = ctx.project_root.join(path);
            if !full.starts_with(&ctx.project_root) {
                return Err(NexusToolError::BadInput("path traversal denied".into()));
            }
            std::fs::read_to_string(&full).map_err(NexusToolError::Io)?
        } else {
            return Err(NexusToolError::BadInput("content or path required".into()));
        };

        let pretty = args.get("pretty").and_then(Value::as_bool).unwrap_or(true);

        match serde_json::from_str::<Value>(&content) {
            Ok(v) => {
                let (typ, elements) = match &v {
                    Value::Array(a) => ("array", Some(a.len())),
                    Value::Object(o) => ("object", Some(o.len())),
                    other => (type_name(other), None),
                };
                let pretty_str = if pretty {
                    Some(serde_json::to_string_pretty(&v)?)
                } else {
                    None
                };
                Ok(json!({
                    "ok": true,
                    "valid": true,
                    "type": typ,
                    "size": content.len(),
                    "elements": elements,
                    "pretty": pretty_str,
                }))
            }
            Err(e) => Ok(json!({
                "ok": true,
                "valid": false,
                "error": e.to_string(),
                "line": e.line(),
                "column": e.column(),
                "size": content.len(),
            })),
        }
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "content": {"type": "string"},
                "path": {"type": "string"},
                "pretty": {"type": "boolean"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_parse_valid() {
        let ctx = NexusToolContext::new(
            std::env::temp_dir(),
            uuid::Uuid::nil(),
            uuid::Uuid::nil(),
        );
        let out = JsonParseTool
            .execute(&ctx, &json!({"content": "{\"a\": 1}"}))
            .await
            .unwrap();
        assert_eq!(out["valid"], true);
        assert_eq!(out["type"], "object");
    }

    #[tokio::test]
    async fn test_parse_invalid() {
        let ctx = NexusToolContext::new(
            std::env::temp_dir(),
            uuid::Uuid::nil(),
            uuid::Uuid::nil(),
        );
        let out = JsonParseTool
            .execute(&ctx, &json!({"content": "{broken"}))
            .await
            .unwrap();
        assert_eq!(out["valid"], false);
    }
}
