//! `utility::uuid_generate` — genera UUID v4 (random) in batch.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct UuidGenerateTool;

#[async_trait]
impl NexusToolHandler for UuidGenerateTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let count = args
            .get("count")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .min(100) as usize;
        let hyphenated = args.get("hyphenated").and_then(Value::as_bool).unwrap_or(true);
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            let u = uuid::Uuid::new_v4();
            out.push(if hyphenated {
                u.to_string()
            } else {
                u.simple().to_string()
            });
        }
        Ok(json!({
            "ok": true,
            "count": out.len(),
            "uuids": out,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "count": {"type": "integer", "minimum": 1, "maximum": 100},
                "hyphenated": {"type": "boolean"}
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
    async fn test_uuid_gen() {
        let ctx = NexusToolContext::new(
            std::env::temp_dir(),
            uuid::Uuid::nil(),
            uuid::Uuid::nil(),
        );
        let out = UuidGenerateTool
            .execute(&ctx, &json!({"count": 3}))
            .await
            .unwrap();
        assert_eq!(out["count"], 3);
        assert_eq!(out["uuids"].as_array().unwrap().len(), 3);
    }
}
