//! `code_analysis::rustc_explain` — `rustc --explain Exxxx`.
//!
//! Ritorna la spiegazione testuale completa di un error code rustc. Utile
//! quando cargo_check segnala un errore E0599 e l'agente vuole contesto
//! senza dover cercare nella documentazione online.
//!
//! Input schema:
//! ```json
//! { "error_code": "E0599" }
//! ```

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct RustcExplainTool;

#[async_trait]
impl NexusToolHandler for RustcExplainTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let error_code = args
            .get("error_code")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("error_code is required".to_string()))?;

        // Validazione formato: deve essere Exxxx (lettera E + 4 cifre)
        if !is_valid_error_code(error_code) {
            return Err(NexusToolError::BadInput(format!(
                "invalid error code format: {} (expected Exxxx)",
                error_code
            )));
        }

        let out = run_cmd(
            "rustc",
            &["--explain", error_code],
            &ctx.project_root,
            ctx.timeout_secs,
        )
        .await?;

        if !out.success() {
            return Err(NexusToolError::Exec {
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }

        Ok(json!({
            "error_code": error_code,
            "explanation": out.stdout,
            "duration_ms": out.duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["error_code"],
            "properties": {
                "error_code": {
                    "type": "string",
                    "description": "Codice errore rustc in formato Exxxx (es. 'E0599')"
                }
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}

fn is_valid_error_code(code: &str) -> bool {
    code.len() == 5
        && code.starts_with('E')
        && code[1..].chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_error_code() {
        assert!(is_valid_error_code("E0599"));
        assert!(is_valid_error_code("E0001"));
        assert!(!is_valid_error_code("e0599"));
        assert!(!is_valid_error_code("E599"));
        assert!(!is_valid_error_code("E05999"));
        assert!(!is_valid_error_code(""));
    }

    #[test]
    fn test_input_schema_requires_error_code() {
        let schema = RustcExplainTool.input_schema();
        assert_eq!(schema["required"][0], "error_code");
    }
}
