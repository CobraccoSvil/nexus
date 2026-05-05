//! `utility::regex_replace` — replace regex su una stringa o file.
//!
//! Input: `{pattern, replacement, content?, path?, max?}`
//! - se `path` è dato (relativo al project_root, read-only mode) viene letto
//!   il file ma NON modificato — l'output contiene il risultato in memoria
//! - `max`: numero massimo di sostituzioni (default = unlimited)
//!
//! Safety: read_only (NON scrive su disco anche se passi `path`).

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct RegexReplaceTool;

#[async_trait]
impl NexusToolHandler for RegexReplaceTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let pattern = args
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("pattern required".into()))?;
        let replacement = args
            .get("replacement")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("replacement required".into()))?;
        let max = args.get("max").and_then(Value::as_u64).map(|v| v as usize);

        let content: String = if let Some(c) = args.get("content").and_then(Value::as_str) {
            c.to_string()
        } else if let Some(p) = args.get("path").and_then(Value::as_str) {
            let full = ctx.project_root.join(p);
            if !full.starts_with(&ctx.project_root) {
                return Err(NexusToolError::BadInput("path traversal denied".into()));
            }
            std::fs::read_to_string(&full).map_err(NexusToolError::Io)?
        } else {
            return Err(NexusToolError::BadInput("content or path required".into()));
        };

        let re = regex::Regex::new(pattern)
            .map_err(|e| NexusToolError::BadInput(format!("bad regex: {}", e)))?;

        let count_before = re.find_iter(&content).count();
        let result: String = if let Some(m) = max {
            re.replacen(&content, m, replacement).into_owned()
        } else {
            re.replace_all(&content, replacement).into_owned()
        };
        let replacements = if let Some(m) = max {
            count_before.min(m)
        } else {
            count_before
        };

        // cap output a 256KB per non flooddare la response
        let truncated = result.len() > 256 * 1024;
        let result_out = if truncated {
            result.chars().take(256 * 1024).collect()
        } else {
            result
        };

        Ok(json!({
            "ok": true,
            "replacements": replacements,
            "result": result_out,
            "truncated": truncated,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern", "replacement"],
            "properties": {
                "pattern": {"type": "string"},
                "replacement": {"type": "string"},
                "content": {"type": "string"},
                "path": {"type": "string"},
                "max": {"type": "integer", "minimum": 1}
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
    async fn test_regex_replace_string() {
        let ctx = NexusToolContext::new(std::env::temp_dir(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = RegexReplaceTool
            .execute(
                &ctx,
                &json!({"pattern": "foo", "replacement": "BAR", "content": "foo foo foo"}),
            )
            .await
            .unwrap();
        assert_eq!(out["replacements"], 3);
        assert_eq!(out["result"], "BAR BAR BAR");
    }

    #[tokio::test]
    async fn test_regex_replace_max() {
        let ctx = NexusToolContext::new(std::env::temp_dir(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = RegexReplaceTool
            .execute(
                &ctx,
                &json!({"pattern": "a", "replacement": "X", "content": "aaaa", "max": 2}),
            )
            .await
            .unwrap();
        assert_eq!(out["result"], "XXaa");
        assert_eq!(out["replacements"], 2);
    }
}
