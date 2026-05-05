//! `utility::base64_encode` — base64 encode di una stringa UTF-8.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use base64::Engine;
use serde_json::{json, Value};

pub struct Base64EncodeTool;

#[async_trait]
impl NexusToolHandler for Base64EncodeTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let input = args
            .get("input")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("input required".into()))?;
        let url_safe = args.get("url_safe").and_then(Value::as_bool).unwrap_or(false);
        let encoded = if url_safe {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input.as_bytes())
        } else {
            base64::engine::general_purpose::STANDARD.encode(input.as_bytes())
        };
        Ok(json!({
            "ok": true,
            "input_bytes": input.len(),
            "output": encoded,
            "url_safe": url_safe,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["input"],
            "properties": {
                "input": {"type": "string"},
                "url_safe": {"type": "boolean"}
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
    async fn test_b64_standard() {
        let ctx = NexusToolContext::new(
            std::env::temp_dir(),
            uuid::Uuid::nil(),
            uuid::Uuid::nil(),
        );
        let out = Base64EncodeTool
            .execute(&ctx, &json!({"input": "hello"}))
            .await
            .unwrap();
        assert_eq!(out["output"], "aGVsbG8=");
    }
}
