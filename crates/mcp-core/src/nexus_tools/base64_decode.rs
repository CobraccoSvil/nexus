//! `utility::base64_decode` — base64 decode a stringa UTF-8.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use base64::Engine;
use serde_json::{json, Value};

pub struct Base64DecodeTool;

#[async_trait]
impl NexusToolHandler for Base64DecodeTool {
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
        let bytes = if url_safe {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(input.as_bytes())
        } else {
            base64::engine::general_purpose::STANDARD.decode(input.as_bytes())
        }
        .map_err(|e| NexusToolError::BadInput(format!("base64 decode failed: {}", e)))?;

        let output = String::from_utf8(bytes.clone()).unwrap_or_else(|_| {
            format!("<binary {}B, not utf-8>", bytes.len())
        });
        Ok(json!({
            "ok": true,
            "bytes": bytes.len(),
            "output": output,
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
    async fn test_b64_decode() {
        let ctx = NexusToolContext::new(
            std::env::temp_dir(),
            uuid::Uuid::nil(),
            uuid::Uuid::nil(),
        );
        let out = Base64DecodeTool
            .execute(&ctx, &json!({"input": "aGVsbG8="}))
            .await
            .unwrap();
        assert_eq!(out["output"], "hello");
    }

    #[tokio::test]
    async fn test_b64_decode_bad() {
        let ctx = NexusToolContext::new(
            std::env::temp_dir(),
            uuid::Uuid::nil(),
            uuid::Uuid::nil(),
        );
        let res = Base64DecodeTool
            .execute(&ctx, &json!({"input": "!!!not-b64!!!"}))
            .await;
        assert!(matches!(res, Err(NexusToolError::BadInput(_))));
    }
}
