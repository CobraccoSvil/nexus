//! `utility::fs_write` — scrive un file dentro il project_root.
//!
//! Input: `{path, content, append?, create_dirs?, max_bytes?}`
//! - `path`: relativo al project_root (path traversal denied)
//! - `content`: stringa UTF-8
//! - `append` (default false): se true appende invece di sovrascrivere
//! - `create_dirs` (default true): crea le dir intermedie se mancanti
//! - `max_bytes` (default 4MB): cap di sicurezza
//!
//! Safety: `can_write_filesystem = true`. È l'unico handler "write" del batch
//! 9G — il dispatcher può rifiutarlo in modalità readonly.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::io::Write;

pub struct FsWriteTool;

const DEFAULT_MAX_BYTES: usize = 4 * 1024 * 1024;

#[async_trait]
impl NexusToolHandler for FsWriteTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("path required".into()))?;
        let content = args
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("content required".into()))?;
        let append = args.get("append").and_then(Value::as_bool).unwrap_or(false);
        let create_dirs = args
            .get("create_dirs")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let max_bytes = args
            .get("max_bytes")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(DEFAULT_MAX_BYTES);

        if content.len() > max_bytes {
            return Err(NexusToolError::BadInput(format!(
                "content too large: {} > {}",
                content.len(),
                max_bytes
            )));
        }

        // Strict check: reject absolute paths and any `..` component up-front.
        // (relying solo su starts_with dopo il join non basta su Windows perché
        // PathBuf non normalizza i componenti `..`).
        let p_in = std::path::Path::new(path);
        if p_in.is_absolute()
            || p_in
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }
        let full = ctx.project_root.join(path);
        if !full.starts_with(&ctx.project_root) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }

        if create_dirs {
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).map_err(NexusToolError::Io)?;
            }
        }

        let bytes_written = content.len();
        if append {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&full)
                .map_err(NexusToolError::Io)?;
            f.write_all(content.as_bytes())
                .map_err(NexusToolError::Io)?;
        } else {
            std::fs::write(&full, content).map_err(NexusToolError::Io)?;
        }

        Ok(json!({
            "ok": true,
            "path": path,
            "mode": if append { "append" } else { "overwrite" },
            "bytes_written": bytes_written,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path", "content"],
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"},
                "append": {"type": "boolean"},
                "create_dirs": {"type": "boolean"},
                "max_bytes": {"type": "integer", "minimum": 1}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: false,
            can_write_filesystem: true,
            can_execute_subproc: false,
            network_egress: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_fs_write_overwrite() {
        let tmp = std::env::temp_dir().join(format!("fsw_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let ctx = NexusToolContext::new(tmp.clone(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = FsWriteTool
            .execute(&ctx, &json!({"path": "a/b/x.txt", "content": "hello"}))
            .await
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["bytes_written"], 5);
        let v = std::fs::read_to_string(tmp.join("a/b/x.txt")).unwrap();
        assert_eq!(v, "hello");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_fs_write_append() {
        let tmp = std::env::temp_dir().join(format!("fsw2_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let ctx = NexusToolContext::new(tmp.clone(), uuid::Uuid::nil(), uuid::Uuid::nil());
        FsWriteTool
            .execute(&ctx, &json!({"path": "x.txt", "content": "a"}))
            .await
            .unwrap();
        FsWriteTool
            .execute(
                &ctx,
                &json!({"path": "x.txt", "content": "b", "append": true}),
            )
            .await
            .unwrap();
        let v = std::fs::read_to_string(tmp.join("x.txt")).unwrap();
        assert_eq!(v, "ab");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_traversal_denied() {
        let tmp = std::env::temp_dir().join(format!("fsw3_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let ctx = NexusToolContext::new(tmp.clone(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let res = FsWriteTool
            .execute(&ctx, &json!({"path": "../etc/passwd", "content": "x"}))
            .await;
        assert!(res.is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
