//! `utility::hash_content` — SHA-256 di una stringa o di un file.
//!
//! Input: `{content | path, algo?}` con algo in `sha256` (default) | `sha512`.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use sha2::{Digest, Sha256, Sha512};

pub struct HashContentTool;

#[async_trait]
impl NexusToolHandler for HashContentTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let algo = args.get("algo").and_then(Value::as_str).unwrap_or("sha256");
        let bytes: Vec<u8> = if let Some(c) = args.get("content").and_then(Value::as_str) {
            c.as_bytes().to_vec()
        } else if let Some(path) = args.get("path").and_then(Value::as_str) {
            let full = ctx.project_root.join(path);
            if !full.starts_with(&ctx.project_root) {
                return Err(NexusToolError::BadInput("path traversal denied".into()));
            }
            std::fs::read(&full).map_err(NexusToolError::Io)?
        } else {
            return Err(NexusToolError::BadInput("content or path required".into()));
        };

        let (hex, used) = match algo {
            "sha256" => {
                let mut h = Sha256::new();
                h.update(&bytes);
                (format!("{:x}", h.finalize()), "sha256")
            }
            "sha512" => {
                let mut h = Sha512::new();
                h.update(&bytes);
                (format!("{:x}", h.finalize()), "sha512")
            }
            other => {
                return Err(NexusToolError::BadInput(format!(
                    "unsupported algo '{}': use sha256 | sha512",
                    other
                )))
            }
        };

        Ok(json!({
            "ok": true,
            "algo": used,
            "bytes": bytes.len(),
            "hex": hex,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "content": {"type": "string"},
                "path": {"type": "string"},
                "algo": {"type": "string", "enum": ["sha256", "sha512"]}
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
    async fn test_sha256_known() {
        let ctx = NexusToolContext::new(
            std::env::temp_dir(),
            uuid::Uuid::nil(),
            uuid::Uuid::nil(),
        );
        let out = HashContentTool
            .execute(&ctx, &json!({"content": "hello"}))
            .await
            .unwrap();
        assert_eq!(out["hex"], "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
    }
}
