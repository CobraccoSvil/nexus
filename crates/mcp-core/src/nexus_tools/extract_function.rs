//! `refactoring::extract_function` — estrae un range di righe in una nuova
//! funzione.
//!
//! Versione **mechanical** (no semantica):
//! 1. Legge il file target
//! 2. Estrae le righe [start_line, end_line] inclusive
//! 3. Genera una function wrapper nel linguaggio rilevato (Rust/TS/JS/Python)
//! 4. Ritorna un patch preview `{new_function, call_site}`
//!
//! L'utente deve sostituire il blocco manualmente (o con un secondo tool di
//! patching). Questo è intenzionale: l'extract corretto di variabili
//! inbound/outbound richiede analisi scope che solo un LSP può fare. Per
//! ora restituiamo lo scaffold della funzione pronta da rifinire.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use mcp_ast::detect_language;
use serde_json::{json, Value};

pub struct ExtractFunctionTool;

fn render_wrapper(lang: &str, name: &str, body: &str) -> (String, String) {
    match lang {
        "rust" => (
            format!(
                "fn {name}() {{\n{body}\n}}\n",
                name = name,
                body = indent(body, "    ")
            ),
            format!("{name}();", name = name),
        ),
        "typescript" | "javascript" => (
            format!(
                "function {name}() {{\n{body}\n}}\n",
                name = name,
                body = indent(body, "  ")
            ),
            format!("{name}();", name = name),
        ),
        "python" => (
            format!(
                "def {name}():\n{body}\n",
                name = name,
                body = indent(body, "    ")
            ),
            format!("{name}()", name = name),
        ),
        _ => (
            format!("# Unsupported language — raw body:\n{body}", body = body),
            String::new(),
        ),
    }
}

fn indent(block: &str, prefix: &str) -> String {
    block
        .lines()
        .map(|l| format!("{}{}", prefix, l))
        .collect::<Vec<_>>()
        .join("\n")
}

#[async_trait]
impl NexusToolHandler for ExtractFunctionTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("path required".into()))?;
        let start_line = args
            .get("start_line")
            .and_then(Value::as_u64)
            .ok_or_else(|| NexusToolError::BadInput("start_line required".into()))?
            as usize;
        let end_line = args
            .get("end_line")
            .and_then(Value::as_u64)
            .ok_or_else(|| NexusToolError::BadInput("end_line required".into()))?
            as usize;
        let new_name = args
            .get("new_name")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("new_name required".into()))?
            .to_string();

        if start_line == 0 || end_line < start_line {
            return Err(NexusToolError::BadInput(
                "start_line must be >=1 and <= end_line".into(),
            ));
        }
        let ident_re = regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap();
        if !ident_re.is_match(&new_name) {
            return Err(NexusToolError::BadInput(format!(
                "new_name '{}' is not a valid identifier",
                new_name
            )));
        }

        let full = ctx.project_root.join(path);
        if !full.starts_with(&ctx.project_root) {
            return Err(NexusToolError::BadInput("path traversal denied".into()));
        }
        let content = std::fs::read_to_string(&full).map_err(NexusToolError::Io)?;
        let lines: Vec<&str> = content.lines().collect();
        if end_line > lines.len() {
            return Err(NexusToolError::BadInput(format!(
                "end_line {} exceeds file length {}",
                end_line,
                lines.len()
            )));
        }
        let body_lines = &lines[start_line - 1..end_line];
        let body = body_lines.join("\n");
        let lang = detect_language(path);

        let (new_fn, call_site) = render_wrapper(lang, &new_name, &body);

        Ok(json!({
            "ok": true,
            "language": lang,
            "file_path": path,
            "range": {"start_line": start_line, "end_line": end_line},
            "new_function": new_fn,
            "call_site": call_site,
            "original_body": body,
            "note": "Mechanical extract. Review inbound/outbound variables and parameter passing manually.",
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path", "start_line", "end_line", "new_name"],
            "properties": {
                "path": {"type": "string"},
                "start_line": {"type": "integer", "minimum": 1},
                "end_line": {"type": "integer", "minimum": 1},
                "new_name": {"type": "string"}
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
    fn test_indent() {
        assert_eq!(indent("a\nb", "  "), "  a\n  b");
    }

    #[test]
    fn test_render_rust() {
        let (fn_src, call) = render_wrapper("rust", "foo", "let x = 1;");
        assert!(fn_src.contains("fn foo()"));
        assert!(fn_src.contains("    let x = 1;"));
        assert_eq!(call, "foo();");
    }

    #[test]
    fn test_render_python() {
        let (fn_src, call) = render_wrapper("python", "bar", "x = 1");
        assert!(fn_src.contains("def bar():"));
        assert_eq!(call, "bar()");
    }

    #[tokio::test]
    async fn test_extract_from_file() {
        let tmp = std::env::temp_dir().join(format!("ef_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("x.rs"),
            "fn main() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n}\n",
        )
        .unwrap();
        let ctx = NexusToolContext::new(tmp.clone(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = ExtractFunctionTool
            .execute(
                &ctx,
                &json!({
                    "path": "x.rs",
                    "start_line": 2,
                    "end_line": 3,
                    "new_name": "setup_values"
                }),
            )
            .await
            .unwrap();
        assert_eq!(out["ok"], true);
        assert!(out["new_function"]
            .as_str()
            .unwrap()
            .contains("fn setup_values"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_safety_readonly() {
        assert!(ExtractFunctionTool.safety().read_only);
    }
}
