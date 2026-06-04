//! `code_analysis::ast_parse` — parsing AST di un file sorgente.
//!
//! Utilizza `mcp_ast::index_source` per estrarre simboli, imports, e
//! line_count da file Rust/TypeScript/JavaScript/Python/Go/Java.
//!
//! Input:
//! - `path` (required): file relativo alla project_root
//! - oppure `content` + `language` inline
//!
//! Output: `{language, symbols, imports, line_count}`.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use mcp_ast::{detect_language, index_source};
use serde_json::{json, Value};

pub struct AstParseTool;

#[async_trait]
impl NexusToolHandler for AstParseTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let (content, file_path) = if let Some(c) = args.get("content").and_then(Value::as_str) {
            let lang_hint = args
                .get("language")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let pseudo_path = format!("inline.{}", lang_ext(lang_hint));
            (c.to_string(), pseudo_path)
        } else {
            let path = args
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| NexusToolError::BadInput("path or content required".into()))?;
            let full = ctx.project_root.join(path);
            if !full.starts_with(&ctx.project_root) {
                return Err(NexusToolError::BadInput("path traversal denied".into()));
            }
            let c = std::fs::read_to_string(&full).map_err(NexusToolError::Io)?;
            (c, path.to_string())
        };

        let index = index_source(&file_path, &content);
        // Serializza via serde (mcp_ast::AstIndex è Serialize)
        let serialized = serde_json::to_value(&index).map_err(NexusToolError::Serde)?;
        Ok(json!({
            "ok": true,
            "file_path": index.file_path,
            "language": index.language,
            "line_count": index.line_count,
            "symbols_count": index.symbols.len(),
            "imports_count": index.imports.len(),
            "index": serialized,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path relative to project root"},
                "content": {"type": "string", "description": "Inline source content"},
                "language": {"type": "string", "enum": ["rust","typescript","javascript","python","go","java"]}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}

fn lang_ext(lang: &str) -> &'static str {
    match lang {
        "rust" => "rs",
        "typescript" => "ts",
        "javascript" => "js",
        "python" => "py",
        "go" => "go",
        "java" => "java",
        _ => "txt",
    }
}

#[allow(dead_code)]
fn _use_detect() {
    // Keep mcp_ast::detect_language in the public surface for doc references.
    let _ = detect_language("x.rs");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_parse_inline_rust() {
        let ctx = NexusToolContext::new(std::env::temp_dir(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let args = json!({
            "content": "pub fn hello() {}\npub struct Foo;",
            "language": "rust"
        });
        let out = AstParseTool.execute(&ctx, &args).await.unwrap();
        assert_eq!(out["language"], "rust");
        assert!(out["symbols_count"].as_u64().unwrap() >= 2);
    }

    #[test]
    fn test_lang_ext() {
        assert_eq!(lang_ext("rust"), "rs");
        assert_eq!(lang_ext("python"), "py");
        assert_eq!(lang_ext("unknown_x"), "txt");
    }

    #[test]
    fn test_safety_readonly() {
        assert!(AstParseTool.safety().read_only);
    }
}
