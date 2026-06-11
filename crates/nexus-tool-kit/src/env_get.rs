//! `utility::env_get` — legge variabili d'ambiente del processo (con whitelist).
//!
//! Input: `{names: [..], allow_secrets?}`
//! - `names`: array di nomi di env var da leggere
//! - `allow_secrets` (default false): se false, **maschera** i valori per nomi
//!   che contengono `SECRET|PASSWORD|TOKEN|KEY|API` (case-insensitive)
//!
//! Output: `{values: {NAME: value_or_masked, ...}, masked: [...]}`.
//!
//! Safety: read_only — accede al PATH/env del processo mcp-core.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Map, Value};

pub struct EnvGetTool;

const SENSITIVE_TOKENS: &[&str] = &["SECRET", "PASSWORD", "TOKEN", "KEY", "API"];

fn is_sensitive(name: &str) -> bool {
    let upper = name.to_uppercase();
    SENSITIVE_TOKENS.iter().any(|t| upper.contains(t))
}

#[async_trait]
impl NexusToolHandler for EnvGetTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let names: Vec<String> = args
            .get("names")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        if names.is_empty() {
            return Err(NexusToolError::BadInput("names array required".into()));
        }
        let allow_secrets = args
            .get("allow_secrets")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let mut values = Map::new();
        let mut masked = Vec::new();
        let mut missing = Vec::new();
        for name in &names {
            match std::env::var(name) {
                Ok(v) => {
                    if !allow_secrets && is_sensitive(name) {
                        let masked_val = if v.is_empty() {
                            "".to_string()
                        } else if v.len() <= 4 {
                            "****".to_string()
                        } else {
                            format!("{}…(len={})", &v[..2], v.len())
                        };
                        values.insert(name.clone(), Value::String(masked_val));
                        masked.push(name.clone());
                    } else {
                        values.insert(name.clone(), Value::String(v));
                    }
                }
                Err(_) => {
                    missing.push(name.clone());
                }
            }
        }

        Ok(json!({
            "ok": true,
            "values": Value::Object(values),
            "missing": missing,
            "masked": masked,
            "allow_secrets": allow_secrets,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["names"],
            "properties": {
                "names": {"type": "array", "items": {"type": "string"}, "minItems": 1},
                "allow_secrets": {"type": "boolean"}
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
    async fn test_env_get_path() {
        std::env::set_var("NEXUS_TEST_PUBLIC", "publicvalue");
        let ctx = NexusToolContext::new(std::env::temp_dir(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = EnvGetTool
            .execute(&ctx, &json!({"names": ["NEXUS_TEST_PUBLIC"]}))
            .await
            .unwrap();
        assert_eq!(out["values"]["NEXUS_TEST_PUBLIC"], "publicvalue");
        assert_eq!(out["masked"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_env_get_masking() {
        std::env::set_var("NEXUS_TEST_API_KEY", "supersecret123");
        let ctx = NexusToolContext::new(std::env::temp_dir(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = EnvGetTool
            .execute(&ctx, &json!({"names": ["NEXUS_TEST_API_KEY"]}))
            .await
            .unwrap();
        let masked_val = out["values"]["NEXUS_TEST_API_KEY"].as_str().unwrap();
        assert!(masked_val != "supersecret123");
        assert!(masked_val.contains('…') || masked_val.contains('*'));
    }

    #[tokio::test]
    async fn test_env_get_missing() {
        let ctx = NexusToolContext::new(std::env::temp_dir(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = EnvGetTool
            .execute(&ctx, &json!({"names": ["NEXUS_DEFINITELY_NOT_SET_XYZ"]}))
            .await
            .unwrap();
        assert_eq!(out["missing"].as_array().unwrap().len(), 1);
    }
}
