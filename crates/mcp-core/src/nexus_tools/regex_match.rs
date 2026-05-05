//! `utility::regex_match` — applica una regex a una stringa o a un file
//! e ritorna tutti i match trovati con posizione.
//!
//! Read-only puro: nessun subprocess, nessuna scrittura FS. Usa la crate
//! `regex` già presente come dipendenza transitiva (secret_scan).
//!
//! Input:
//! ```json
//! {
//!   "pattern": "TODO.*",
//!   "text": "...",          // oppure `file`, non entrambi
//!   "file": "src/main.rs",  // path relativo al project_root
//!   "case_insensitive": true,
//!   "multi_line": true,
//!   "max_matches": 100
//! }
//! ```
//!
//! Output:
//! ```json
//! {
//!   "pattern": "TODO.*",
//!   "matches_count": 3,
//!   "matches": [
//!     {"start": 42, "end": 56, "text": "TODO: rename"},
//!     ...
//!   ]
//! }
//! ```

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct RegexMatchTool;

#[async_trait]
impl NexusToolHandler for RegexMatchTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let pattern = args
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("parametro 'pattern' obbligatorio".into()))?;

        let ci = args
            .get("case_insensitive")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let multi = args
            .get("multi_line")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let max_matches = args
            .get("max_matches")
            .and_then(Value::as_u64)
            .unwrap_or(1000)
            .min(10_000) as usize;

        // Costruisci la regex con flag inline (?i)(?m) per semplicità.
        let mut flags = String::new();
        if ci {
            flags.push('i');
        }
        if multi {
            flags.push('m');
        }
        let wrapped = if flags.is_empty() {
            pattern.to_string()
        } else {
            format!("(?{}){}", flags, pattern)
        };

        let re = regex::Regex::new(&wrapped)
            .map_err(|e| NexusToolError::BadInput(format!("regex non valida: {}", e)))?;

        // Sorgente: `text` inline, oppure lettura file (path relativo al root).
        let (source, source_kind) = if let Some(t) = args.get("text").and_then(Value::as_str) {
            (t.to_string(), "text")
        } else if let Some(f) = args.get("file").and_then(Value::as_str) {
            let path = ctx.project_root.join(f);
            // Evita path traversal fuori dal project_root
            if !path.starts_with(&ctx.project_root) {
                return Err(NexusToolError::BadInput(format!(
                    "path fuori dal project_root: {}",
                    f
                )));
            }
            let content = tokio::fs::read_to_string(&path)
                .await
                .map_err(NexusToolError::Io)?;
            (content, "file")
        } else {
            return Err(NexusToolError::BadInput(
                "richiesto 'text' oppure 'file'".into(),
            ));
        };

        let mut matches: Vec<Value> = Vec::new();
        for m in re.find_iter(&source).take(max_matches) {
            matches.push(json!({
                "start": m.start(),
                "end": m.end(),
                "text": m.as_str(),
            }));
        }

        Ok(json!({
            "pattern": pattern,
            "source": source_kind,
            "matches_count": matches.len(),
            "matches": matches,
            "truncated": matches.len() == max_matches,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": {"type": "string", "description": "Pattern regex (sintassi Rust)"},
                "text": {"type": "string", "description": "Testo inline su cui matchare"},
                "file": {"type": "string", "description": "Path relativo al project_root"},
                "case_insensitive": {"type": "boolean"},
                "multi_line": {"type": "boolean"},
                "max_matches": {"type": "integer", "description": "Limite match (default 1000, max 10000)"}
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
    use uuid::Uuid;

    fn ctx() -> NexusToolContext {
        NexusToolContext::new(std::env::temp_dir(), Uuid::nil(), Uuid::nil())
    }

    #[tokio::test]
    async fn test_regex_match_text_basic() {
        let out = RegexMatchTool
            .execute(&ctx(), &json!({"pattern": r"\d+", "text": "foo 42 bar 13"}))
            .await
            .unwrap();
        assert_eq!(out["matches_count"], 2);
        assert_eq!(out["matches"][0]["text"], "42");
        assert_eq!(out["matches"][1]["text"], "13");
    }

    #[tokio::test]
    async fn test_regex_match_case_insensitive() {
        let out = RegexMatchTool
            .execute(
                &ctx(),
                &json!({"pattern": "todo", "text": "TODO FixMe TODO", "case_insensitive": true}),
            )
            .await
            .unwrap();
        assert_eq!(out["matches_count"], 2);
    }

    #[tokio::test]
    async fn test_regex_match_invalid_pattern() {
        let err = RegexMatchTool
            .execute(&ctx(), &json!({"pattern": "(unclosed", "text": "x"}))
            .await
            .unwrap_err();
        assert!(matches!(err, NexusToolError::BadInput(_)));
    }

    #[tokio::test]
    async fn test_regex_match_missing_source() {
        let err = RegexMatchTool
            .execute(&ctx(), &json!({"pattern": "x"}))
            .await
            .unwrap_err();
        assert!(matches!(err, NexusToolError::BadInput(_)));
    }
}
