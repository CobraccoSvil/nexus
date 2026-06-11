//! `code_analysis::ast_query` — interroga l'AST di un file per simboli che
//! soddisfano un filtro.
//!
//! Appoggiato su `mcp_ast::index_source`, poi filtra i simboli per `kind`
//! e/o `name_pattern` (regex). Utile per domande come "tutte le struct
//! pubbliche di questo file" o "le funzioni async che iniziano con handle_".
//!
//! Input:
//! - `path` (required): file relativo alla project_root
//! - `kind` (optional): uno tra "function","class","method","interface",
//!   "struct","enum","constant","variable","import"
//! - `name_pattern` (optional): regex sui nomi
//! - `visibility` (optional): "public"|"private"

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use mcp_ast::{index_source, SymbolKind, Visibility};
use regex::Regex;
use serde_json::{json, Value};

pub struct AstQueryTool;

fn kind_from_str(s: &str) -> Option<SymbolKind> {
    match s.to_lowercase().as_str() {
        "function" => Some(SymbolKind::Function),
        "class" => Some(SymbolKind::Class),
        "method" => Some(SymbolKind::Method),
        "interface" => Some(SymbolKind::Interface),
        "struct" => Some(SymbolKind::Struct),
        "enum" => Some(SymbolKind::Enum),
        "constant" => Some(SymbolKind::Constant),
        "variable" => Some(SymbolKind::Variable),
        "import" => Some(SymbolKind::Import),
        _ => None,
    }
}

#[async_trait]
impl NexusToolHandler for AstQueryTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("path required".into()))?;
        let full = ctx.project_root.join(path);
        if !full.starts_with(&ctx.project_root) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }
        let content = std::fs::read_to_string(&full).map_err(NexusToolError::Io)?;
        let index = index_source(path, &content);

        let kind_filter = args
            .get("kind")
            .and_then(Value::as_str)
            .and_then(kind_from_str);
        let name_re = match args.get("name_pattern").and_then(Value::as_str) {
            Some(p) => Some(
                Regex::new(p)
                    .map_err(|e| NexusToolError::BadInput(format!("invalid regex: {}", e)))?,
            ),
            None => None,
        };
        let vis_filter = args.get("visibility").and_then(Value::as_str).map(|s| {
            match s.to_lowercase().as_str() {
                "public" => Visibility::Public,
                "private" => Visibility::Private,
                _ => Visibility::Unknown,
            }
        });

        let matches: Vec<Value> = index
            .symbols
            .iter()
            .filter(|s| {
                if let Some(k) = &kind_filter {
                    if &s.kind != k {
                        return false;
                    }
                }
                if let Some(re) = &name_re {
                    if !re.is_match(&s.name) {
                        return false;
                    }
                }
                if let Some(v) = &vis_filter {
                    if &s.visibility != v {
                        return false;
                    }
                }
                true
            })
            .map(|s| {
                json!({
                    "name": s.name,
                    "kind": format!("{:?}", s.kind).to_lowercase(),
                    "line": s.line,
                    "visibility": format!("{:?}", s.visibility).to_lowercase(),
                })
            })
            .collect();

        Ok(json!({
            "ok": true,
            "file_path": path,
            "language": index.language,
            "total_symbols": index.symbols.len(),
            "matches_count": matches.len(),
            "matches": matches,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {"type": "string"},
                "kind": {"type": "string", "enum": ["function","class","method","interface","struct","enum","constant","variable","import"]},
                "name_pattern": {"type": "string", "description": "Regex sui nomi"},
                "visibility": {"type": "string", "enum": ["public", "private"]}
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

    #[test]
    fn test_kind_from_str() {
        assert_eq!(kind_from_str("function"), Some(SymbolKind::Function));
        assert_eq!(kind_from_str("STRUCT"), Some(SymbolKind::Struct));
        assert_eq!(kind_from_str("nonsense"), None);
    }

    #[tokio::test]
    async fn test_query_rust_struct() {
        let tmp = std::env::temp_dir().join(format!("ast_q_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("x.rs"),
            "pub struct Foo;\npub fn bar() {}\nstruct Bazz;",
        )
        .unwrap();
        let ctx = NexusToolContext::new(tmp.clone(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = AstQueryTool
            .execute(&ctx, &json!({"path": "x.rs", "kind": "struct"}))
            .await
            .unwrap();
        let matches = out["matches"].as_array().unwrap();
        assert!(matches.len() >= 2);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_safety_readonly() {
        assert!(AstQueryTool.safety().read_only);
    }
}
