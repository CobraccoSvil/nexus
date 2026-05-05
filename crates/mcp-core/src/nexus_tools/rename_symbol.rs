//! `refactoring::rename_symbol` — rinomina un simbolo in un singolo file.
//!
//! Versione **conservative** (no multi-file cross-module tracking — quello
//! richiede un LSP server come rust-analyzer):
//! 1. Controlla che il simbolo esista nel file target (`mcp_ast::index_source`)
//! 2. Fa una sostituzione regex-based con word-boundary matching
//! 3. Ritorna il contenuto modificato (senza scriverlo — l'utente applica)
//!
//! `apply=true` chiede esplicitamente di scrivere il file in place.
//!
//! Per refactoring cross-file si consiglia di chiamare questo tool su ogni
//! file impattato dopo un `grep -l` della symbol.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use mcp_ast::index_source;
use regex::Regex;
use serde_json::{json, Value};

pub struct RenameSymbolTool;

#[async_trait]
impl NexusToolHandler for RenameSymbolTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("path required".into()))?;
        let old_name = args
            .get("old_name")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("old_name required".into()))?;
        let new_name = args
            .get("new_name")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("new_name required".into()))?;
        let apply = args.get("apply").and_then(Value::as_bool).unwrap_or(false);

        // Validazione: new_name deve essere un identifier valido
        let ident_re = Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap();
        if !ident_re.is_match(new_name) {
            return Err(NexusToolError::BadInput(format!(
                "new_name '{}' is not a valid identifier",
                new_name
            )));
        }
        if old_name == new_name {
            return Err(NexusToolError::BadInput(
                "old_name equals new_name (no-op)".into(),
            ));
        }

        let full = ctx.project_root.join(path);
        if !full.starts_with(&ctx.project_root) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }
        let content = std::fs::read_to_string(&full).map_err(NexusToolError::Io)?;

        // Verifica che il simbolo esista
        let index = index_source(path, &content);
        let exists = index.symbols.iter().any(|s| s.name == old_name);
        if !exists {
            return Ok(json!({
                "ok": false,
                "error": format!("Symbol '{}' not found in {}", old_name, path),
                "available_symbols": index.symbols.iter().map(|s| &s.name).take(20).collect::<Vec<_>>(),
            }));
        }

        // Sostituzione word-boundary
        let pattern = format!(r"\b{}\b", regex::escape(old_name));
        let re = Regex::new(&pattern)
            .map_err(|e| NexusToolError::BadInput(format!("regex build: {}", e)))?;
        let replaced = re.replace_all(&content, new_name).into_owned();
        let occurrences = re.find_iter(&content).count();

        if apply {
            std::fs::write(&full, &replaced).map_err(NexusToolError::Io)?;
        }

        Ok(json!({
            "ok": true,
            "applied": apply,
            "file_path": path,
            "old_name": old_name,
            "new_name": new_name,
            "occurrences": occurrences,
            "new_content": if apply { Value::Null } else { Value::String(replaced) },
            "note": "Single-file rename. For cross-file rename run grep -l <old_name> and invoke rename_symbol per file.",
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path", "old_name", "new_name"],
            "properties": {
                "path": {"type": "string"},
                "old_name": {"type": "string"},
                "new_name": {"type": "string"},
                "apply": {"type": "boolean", "description": "Scrivi il file in place (default false)"}
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
    async fn test_rename_preview() {
        let tmp = std::env::temp_dir().join(format!("rs_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("x.rs"), "pub fn foo() { foo_helper(); }\nfn foo_helper() {}").unwrap();
        let ctx = NexusToolContext::new(tmp.clone(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = RenameSymbolTool
            .execute(
                &ctx,
                &json!({"path": "x.rs", "old_name": "foo", "new_name": "bar"}),
            )
            .await
            .unwrap();
        assert_eq!(out["ok"], true);
        // "foo" appare 1 volta (word boundary esclude foo_helper)
        assert_eq!(out["occurrences"], 1);
        // File non modificato (apply=false)
        let disk = std::fs::read_to_string(tmp.join("x.rs")).unwrap();
        assert!(disk.contains("pub fn foo"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_rename_invalid_new_name() {
        let tmp = std::env::temp_dir().join(format!("rs2_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("x.rs"), "pub fn foo() {}").unwrap();
        let ctx = NexusToolContext::new(tmp.clone(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let res = RenameSymbolTool
            .execute(
                &ctx,
                &json!({"path": "x.rs", "old_name": "foo", "new_name": "9bad"}),
            )
            .await;
        assert!(matches!(res, Err(NexusToolError::BadInput(_))));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_safety_writes() {
        assert!(RenameSymbolTool.safety().can_write_filesystem);
    }
}
