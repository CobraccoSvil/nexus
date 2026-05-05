//! `utility::fs_read` — legge un file dal project_root con line range opzionale.
//!
//! Input: `{path, start_line?, end_line?, max_bytes?}`
//! - `path`: relativo al project_root (path traversal denied)
//! - `start_line` (1-based), `end_line` inclusivi — opzionali
//! - `max_bytes` (default 256KB) — cap di sicurezza per file grandi
//!
//! Output: `{ok, path, line_count, start_line, end_line, truncated, content}`.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct FsReadTool;

const DEFAULT_MAX_BYTES: usize = 256 * 1024;

#[async_trait]
impl NexusToolHandler for FsReadTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("path required".into()))?;
        let start_line = args.get("start_line").and_then(Value::as_u64).map(|v| v as usize);
        let end_line = args.get("end_line").and_then(Value::as_u64).map(|v| v as usize);
        let max_bytes = args
            .get("max_bytes")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(DEFAULT_MAX_BYTES);

        let full = ctx.project_root.join(path);
        // Blocca path traversal sia via componenti `..` sia via starts_with post-join.
        // starts_with da solo non basta: /root/uuid/../../../etc/passwd inizia con /root/uuid.
        if full.components().any(|c| c == std::path::Component::ParentDir)
            || !full.starts_with(&ctx.project_root)
        {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }
        let content = std::fs::read_to_string(&full).map_err(NexusToolError::Io)?;
        let total_lines = content.lines().count();

        let lines: Vec<&str> = content.lines().collect();
        let start = start_line.unwrap_or(1).max(1);
        let end = end_line.unwrap_or(total_lines).min(total_lines);

        let selected: String = if start > end {
            String::new()
        } else {
            lines[start - 1..end].join("\n")
        };

        let truncated = selected.len() > max_bytes;
        let out_content = if truncated {
            selected.chars().take(max_bytes).collect()
        } else {
            selected
        };

        Ok(json!({
            "ok": true,
            "path": path,
            "line_count": total_lines,
            "start_line": start,
            "end_line": end,
            "bytes": out_content.len(),
            "truncated": truncated,
            "content": out_content,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {"type": "string"},
                "start_line": {"type": "integer", "minimum": 1},
                "end_line": {"type": "integer", "minimum": 1},
                "max_bytes": {"type": "integer", "minimum": 1}
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
    async fn test_fs_read_range() {
        let tmp = std::env::temp_dir().join(format!("fsr_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("x.txt"), "l1\nl2\nl3\nl4\nl5\n").unwrap();
        let ctx = NexusToolContext::new(tmp.clone(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = FsReadTool
            .execute(&ctx, &json!({"path": "x.txt", "start_line": 2, "end_line": 4}))
            .await
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["line_count"], 5);
        assert_eq!(out["content"], "l2\nl3\nl4");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_fs_read_traversal_denied() {
        let tmp = std::env::temp_dir().join(format!("fsr2_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let ctx = NexusToolContext::new(tmp.clone(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let res = FsReadTool
            .execute(&ctx, &json!({"path": "../../../etc/passwd"}))
            .await;
        assert!(matches!(res, Err(NexusToolError::BadInput(_)) | Err(NexusToolError::Io(_))));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
