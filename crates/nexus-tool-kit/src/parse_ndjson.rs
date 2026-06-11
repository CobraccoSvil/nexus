//! Helper condiviso: parser del flusso NDJSON prodotto da
//! `cargo build|check|test|clippy --message-format=json`.
//!
//! Tutti questi comandi producono lo stesso formato "compiler-message" con
//! campi `level`, `message`, `spans[].is_primary`, `spans[].file_name`,
//! `spans[].line_start`. Centralizzare qui il parsing evita ripetizioni nei
//! singoli handler e mantiene il comportamento coerente quando rustc cambia
//! schema.

use serde_json::{json, Value};

/// Estrae errori e warning strutturati da un flusso NDJSON cargo.
///
/// Ritorna `(errors, warnings)` come coppie di `Vec<Value>` con entry della
/// forma `{"file": "...", "line": N, "message": "..."}`.
pub fn extract_cargo_diagnostics(ndjson: &str) -> (Vec<Value>, Vec<Value>) {
    let mut errors: Vec<Value> = Vec::new();
    let mut warnings: Vec<Value> = Vec::new();

    for line in ndjson.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parsed: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if parsed.get("reason").and_then(Value::as_str) != Some("compiler-message") {
            continue;
        }
        let message = match parsed.get("message") {
            Some(m) => m,
            None => continue,
        };
        let level = message.get("level").and_then(Value::as_str).unwrap_or("");
        let text = message
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let (file, line_no) = message
            .get("spans")
            .and_then(Value::as_array)
            .and_then(|spans| {
                spans
                    .iter()
                    .find(|s| s.get("is_primary") == Some(&json!(true)))
            })
            .map(|span| {
                (
                    span.get("file_name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    span.get("line_start").and_then(Value::as_u64).unwrap_or(0),
                )
            })
            .unwrap_or_default();

        let entry = json!({
            "file": file,
            "line": line_no,
            "message": text,
        });

        match level {
            "error" | "error: internal compiler error" => errors.push(entry),
            "warning" => warnings.push(entry),
            _ => {}
        }
    }

    (errors, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_diagnostics() {
        let stream = r#"
{"reason":"compiler-message","message":{"level":"error","message":"cannot find type","spans":[{"is_primary":true,"file_name":"src/lib.rs","line_start":10}]}}
{"reason":"compiler-message","message":{"level":"warning","message":"unused var","spans":[{"is_primary":true,"file_name":"src/main.rs","line_start":5}]}}
{"reason":"build-finished","success":false}
"#;
        let (errors, warnings) = extract_cargo_diagnostics(stream);
        assert_eq!(errors.len(), 1);
        assert_eq!(warnings.len(), 1);
        assert_eq!(errors[0]["file"], "src/lib.rs");
        assert_eq!(errors[0]["line"], 10);
        assert_eq!(warnings[0]["message"], "unused var");
    }

    #[test]
    fn test_ignores_invalid_json() {
        let stream = "not-json\n{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",\"message\":\"x\"}}\n";
        let (errors, _) = extract_cargo_diagnostics(stream);
        assert_eq!(errors.len(), 1);
    }
}
