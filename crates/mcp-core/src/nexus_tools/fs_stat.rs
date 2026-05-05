//! `utility::fs_stat` — metadata di un file o directory del project_root.
//!
//! Input: `{path}` (relativo al project_root)
//! Output: `{exists, type, size, modified_unix, readonly, name, parent}`.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct FsStatTool;

#[async_trait]
impl NexusToolHandler for FsStatTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("path required".into()))?;

        let full = ctx.project_root.join(path);
        if !full.starts_with(&ctx.project_root) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }

        if !full.exists() {
            return Ok(json!({
                "ok": true,
                "exists": false,
                "path": path,
            }));
        }

        let meta = std::fs::metadata(&full).map_err(NexusToolError::Io)?;
        let modified_unix = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let age_secs = now.saturating_sub(modified_unix);

        let kind = if meta.is_dir() {
            "dir"
        } else if meta.is_file() {
            "file"
        } else {
            "other"
        };

        let name = full
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parent = full
            .parent()
            .and_then(|p| p.strip_prefix(&ctx.project_root).ok())
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();

        Ok(json!({
            "ok": true,
            "exists": true,
            "type": kind,
            "size": meta.len(),
            "modified_unix": modified_unix,
            "age_secs": age_secs,
            "readonly": meta.permissions().readonly(),
            "name": name,
            "parent": parent,
            "path": path,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {"path": {"type": "string"}}
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
    async fn test_fs_stat_file() {
        let tmp = std::env::temp_dir().join(format!("fst_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("hello.txt"), "abcde").unwrap();
        let ctx = NexusToolContext::new(tmp.clone(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = FsStatTool
            .execute(&ctx, &json!({"path": "hello.txt"}))
            .await
            .unwrap();
        assert_eq!(out["exists"], true);
        assert_eq!(out["type"], "file");
        assert_eq!(out["size"], 5);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_fs_stat_missing() {
        let tmp = std::env::temp_dir().join(format!("fst2_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let ctx = NexusToolContext::new(tmp.clone(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = FsStatTool
            .execute(&ctx, &json!({"path": "nope.txt"}))
            .await
            .unwrap();
        assert_eq!(out["exists"], false);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
