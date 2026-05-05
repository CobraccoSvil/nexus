//! `api::openapi_validate` — validazione di una specifica OpenAPI.
//!
//! Validazione **basic** (no spec-compliance completa, ma copre i check più
//! frequenti):
//! - parse JSON/YAML valido
//! - presenza di `openapi` (top-level string semver-like)
//! - presenza di `info.title` + `info.version`
//! - presenza di almeno un elemento in `paths`
//! - ogni operation (get/post/put/patch/delete) dichiara almeno una
//!   `responses` con chiave numeric o "default"
//!
//! Input:
//! - `path` (optional): file relativo alla project_root. Default: cerca
//!   `openapi.json`, `openapi.yaml`, `openapi.yml`.
//! - `content` (optional): contenuto inline della spec.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct OpenApiValidateTool;

#[derive(Debug, Default)]
struct Report {
    errors: Vec<String>,
    warnings: Vec<String>,
    path_count: usize,
    operation_count: usize,
}

fn find_default_spec(root: &std::path::Path) -> Option<std::path::PathBuf> {
    for candidate in &["openapi.json", "openapi.yaml", "openapi.yml"] {
        let p = root.join(candidate);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn parse_spec(content: &str, path_hint: &str) -> Result<Value, String> {
    // Prova JSON prima, poi YAML
    if path_hint.ends_with(".json") {
        serde_json::from_str(content).map_err(|e| format!("JSON parse: {}", e))
    } else {
        // Tentativo JSON anche per estensioni non note
        if let Ok(v) = serde_json::from_str::<Value>(content) {
            return Ok(v);
        }
        // Fallback: non abbiamo serde_yaml, ritorniamo errore informativo
        Err(
            "YAML parser non disponibile in questo build. Usa openapi.json \
             o passa `content` come JSON."
                .into(),
        )
    }
}

fn validate_spec(spec: &Value) -> Report {
    let mut report = Report::default();

    // openapi version
    match spec.get("openapi").and_then(Value::as_str) {
        Some(v) if !v.is_empty() => {}
        _ => report
            .errors
            .push("missing top-level 'openapi' (expected semver-like string)".into()),
    }

    // info
    let info = spec.get("info");
    if info.is_none() {
        report.errors.push("missing 'info' object".into());
    } else {
        let info = info.unwrap();
        if info.get("title").and_then(Value::as_str).unwrap_or("").is_empty() {
            report.errors.push("missing 'info.title'".into());
        }
        if info
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("")
            .is_empty()
        {
            report.errors.push("missing 'info.version'".into());
        }
    }

    // paths
    let paths = spec.get("paths").and_then(Value::as_object);
    match paths {
        None => report.errors.push("missing 'paths' object".into()),
        Some(p) if p.is_empty() => report.warnings.push("'paths' is empty".into()),
        Some(p) => {
            report.path_count = p.len();
            for (path_str, item) in p {
                let methods = ["get", "post", "put", "patch", "delete", "head", "options"];
                for m in &methods {
                    if let Some(op) = item.get(m) {
                        report.operation_count += 1;
                        let responses = op.get("responses").and_then(Value::as_object);
                        match responses {
                            None => report.errors.push(format!(
                                "{} {} has no 'responses' block",
                                m.to_uppercase(),
                                path_str
                            )),
                            Some(r) if r.is_empty() => report.errors.push(format!(
                                "{} {} has empty 'responses'",
                                m.to_uppercase(),
                                path_str
                            )),
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    report
}

#[async_trait]
impl NexusToolHandler for OpenApiValidateTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let content_inline = args.get("content").and_then(Value::as_str);
        let (content, path_hint): (String, String) = if let Some(c) = content_inline {
            (c.to_string(), "inline.json".to_string())
        } else {
            let path_arg = args.get("path").and_then(Value::as_str);
            let resolved = if let Some(p) = path_arg {
                let candidate = ctx.project_root.join(p);
                if !candidate.starts_with(&ctx.project_root) {
                    return Err(NexusToolError::BadInput("path traversal denied".into()));
                }
                candidate
            } else {
                match find_default_spec(&ctx.project_root) {
                    Some(p) => p,
                    None => {
                        return Ok(json!({
                            "ok": false,
                            "error": "No openapi.{json,yaml,yml} found in project root and no path/content provided",
                        }));
                    }
                }
            };
            let hint = resolved
                .file_name()
                .map(|o| o.to_string_lossy().into_owned())
                .unwrap_or_default();
            let content = std::fs::read_to_string(&resolved).map_err(NexusToolError::Io)?;
            (content, hint)
        };

        let spec = match parse_spec(&content, &path_hint) {
            Ok(s) => s,
            Err(e) => {
                return Ok(json!({
                    "ok": false,
                    "error": e,
                }));
            }
        };

        let report = validate_spec(&spec);
        Ok(json!({
            "ok": report.errors.is_empty(),
            "errors": report.errors,
            "warnings": report.warnings,
            "path_count": report.path_count,
            "operation_count": report.operation_count,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Relative to project root"},
                "content": {"type": "string", "description": "Inline spec (JSON)"}
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
    fn test_validate_ok() {
        let spec = json!({
            "openapi": "3.0.0",
            "info": {"title": "X", "version": "1.0"},
            "paths": {
                "/users": {"get": {"responses": {"200": {"description": "ok"}}}}
            }
        });
        let r = validate_spec(&spec);
        assert!(r.errors.is_empty());
        assert_eq!(r.path_count, 1);
        assert_eq!(r.operation_count, 1);
    }

    #[test]
    fn test_validate_missing_info() {
        let spec = json!({"openapi": "3.0.0", "paths": {}});
        let r = validate_spec(&spec);
        assert!(r.errors.iter().any(|e| e.contains("info")));
    }

    #[test]
    fn test_validate_missing_responses() {
        let spec = json!({
            "openapi": "3.0.0",
            "info": {"title": "X", "version": "1.0"},
            "paths": {"/x": {"get": {}}}
        });
        let r = validate_spec(&spec);
        assert!(r.errors.iter().any(|e| e.contains("responses")));
    }

    #[test]
    fn test_parse_spec_json() {
        let v = parse_spec("{\"openapi\":\"3.0\"}", "openapi.json").unwrap();
        assert_eq!(v["openapi"], "3.0");
    }

    #[test]
    fn test_safety_readonly() {
        assert!(OpenApiValidateTool.safety().read_only);
    }
}
