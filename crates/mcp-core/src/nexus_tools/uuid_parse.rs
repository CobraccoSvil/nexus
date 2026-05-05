//! `utility::uuid_parse` — valida e descrive un UUID stringa.
//!
//! Output: `{valid, version, variant, hyphenated, urn, hex}`.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use uuid::Uuid;

pub struct UuidParseTool;

#[async_trait]
impl NexusToolHandler for UuidParseTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let input = args
            .get("input")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("input required".into()))?;
        match Uuid::parse_str(input) {
            Ok(u) => Ok(json!({
                "ok": true,
                "valid": true,
                "version": u.get_version_num(),
                "variant": format!("{:?}", u.get_variant()),
                "hyphenated": u.hyphenated().to_string(),
                "simple": u.simple().to_string(),
                "urn": u.urn().to_string(),
                "is_nil": u.is_nil(),
            })),
            Err(e) => Ok(json!({
                "ok": true,
                "valid": false,
                "error": e.to_string(),
            })),
        }
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["input"],
            "properties": {"input": {"type": "string"}}
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
    async fn test_uuid_parse_valid() {
        let ctx = NexusToolContext::new(std::env::temp_dir(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = UuidParseTool
            .execute(
                &ctx,
                &json!({"input": "550e8400-e29b-41d4-a716-446655440000"}),
            )
            .await
            .unwrap();
        assert_eq!(out["valid"], true);
        assert_eq!(out["version"], 4);
    }

    #[tokio::test]
    async fn test_uuid_parse_invalid() {
        let ctx = NexusToolContext::new(std::env::temp_dir(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = UuidParseTool
            .execute(&ctx, &json!({"input": "not-a-uuid"}))
            .await
            .unwrap();
        assert_eq!(out["valid"], false);
    }
}
