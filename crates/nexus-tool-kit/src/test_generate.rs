//! `testing::test_generate` — generazione test da firma di funzione.
//!
//! Questa è una versione **scaffold** che:
//! 1. Parsifica il file target con `mcp_ast::index_source` per estrarre i
//!    simboli funzione
//! 2. Per ogni funzione trovata genera uno scheletro di test unit in base
//!    al linguaggio rilevato (Rust: `#[test]`, JS: `test(...)`,
//!    Python: `def test_...`)
//! 3. Ritorna il codice generato (non lo scrive)
//!
//! La logica è intenzionalmente meccanica (nessuna integrazione LLM): copre
//! il "quick scaffold" per evitare boilerplate. Un futuro upgrade LLM-based
//! può estendere questa stessa interfaccia.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use mcp_ast::{index_source, SymbolKind};
use serde_json::{json, Value};

pub struct TestGenerateTool;

fn render_rust_test(fn_name: &str) -> String {
    format!(
        "#[test]\nfn test_{name}() {{\n    // TODO: arrange\n    // TODO: act — chiama {name}(...)\n    // TODO: assert\n}}\n",
        name = fn_name
    )
}

fn render_js_test(fn_name: &str) -> String {
    format!(
        "test('{name} should behave correctly', () => {{\n  // TODO: arrange\n  // TODO: act — chiama {name}(...)\n  // TODO: expect(...)\n}});\n",
        name = fn_name
    )
}

fn render_py_test(fn_name: &str) -> String {
    format!(
        "def test_{name}():\n    # TODO: arrange\n    # TODO: act — chiama {name}(...)\n    # TODO: assert\n    pass\n",
        name = fn_name
    )
}

fn render_for_lang(lang: &str, fn_name: &str) -> Option<String> {
    match lang {
        "rust" => Some(render_rust_test(fn_name)),
        "javascript" | "typescript" => Some(render_js_test(fn_name)),
        "python" => Some(render_py_test(fn_name)),
        _ => None,
    }
}

#[async_trait]
impl NexusToolHandler for TestGenerateTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("path required".into()))?;
        let full = ctx.project_root.join(path);
        if !full.starts_with(&ctx.project_root) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }
        let function_filter = args
            .get("function")
            .and_then(Value::as_str)
            .map(String::from);
        let max = args
            .get("max")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .min(100) as usize;

        let content = std::fs::read_to_string(&full).map_err(NexusToolError::Io)?;
        let index = index_source(path, &content);

        if !matches!(
            index.language.as_str(),
            "rust" | "javascript" | "typescript" | "python"
        ) {
            return Ok(json!({
                "ok": false,
                "error": format!("Language '{}' not supported by scaffolder", index.language),
                "supported": ["rust", "javascript", "typescript", "python"],
            }));
        }

        let mut generated: Vec<Value> = Vec::new();
        for sym in index.symbols.iter() {
            if sym.kind != SymbolKind::Function {
                continue;
            }
            if let Some(f) = &function_filter {
                if &sym.name != f {
                    continue;
                }
            }
            if let Some(code) = render_for_lang(&index.language, &sym.name) {
                generated.push(json!({
                    "function": sym.name,
                    "code": code,
                }));
            }
            if generated.len() >= max {
                break;
            }
        }

        Ok(json!({
            "ok": true,
            "engine": "scaffold",
            "language": index.language,
            "file_path": path,
            "functions_found": index.symbols.iter().filter(|s| s.kind == SymbolKind::Function).count(),
            "generated_count": generated.len(),
            "tests": generated,
            "note": "Scaffold output. For full test generation use LLM agent integration (future).",
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {"type": "string"},
                "function": {"type": "string", "description": "Generate solo per questa funzione"},
                "max": {"type": "integer", "minimum": 1, "maximum": 100}
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
    fn test_render_rust() {
        let s = render_rust_test("foo");
        assert!(s.contains("#[test]"));
        assert!(s.contains("test_foo"));
    }

    #[test]
    fn test_render_py() {
        let s = render_py_test("bar");
        assert!(s.starts_with("def test_bar"));
    }

    #[tokio::test]
    async fn test_generate_from_rust_source() {
        let tmp = std::env::temp_dir().join(format!("tg_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("lib.rs"), "pub fn foo() {}\npub fn bar() {}").unwrap();
        let ctx = NexusToolContext::new(tmp.clone(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = TestGenerateTool
            .execute(&ctx, &json!({"path": "lib.rs"}))
            .await
            .unwrap();
        assert_eq!(out["ok"], true);
        assert!(out["generated_count"].as_u64().unwrap() >= 2);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_safety_readonly() {
        assert!(TestGenerateTool.safety().read_only);
    }
}
