//! `testing::test_coverage` — wrapper di `cargo llvm-cov --json --summary-only`.
//!
//! Richiede `cargo install cargo-llvm-cov` e il component `llvm-tools-preview`.
//! Ritorna la coverage aggregata del workspace (linee, funzioni, regioni)
//! parsando il JSON di llvm-cov.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct TestCoverageTool;

#[async_trait]
impl NexusToolHandler for TestCoverageTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let workspace_member = args
            .get("workspace_member")
            .and_then(Value::as_str)
            .map(String::from);

        let mut cmd: Vec<String> =
            vec!["llvm-cov".into(), "--json".into(), "--summary-only".into()];
        if let Some(m) = &workspace_member {
            cmd.push("-p".into());
            cmd.push(m.clone());
        }
        let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        let out = run_cmd("cargo", &refs, &ctx.project_root, ctx.timeout_secs).await?;

        let parsed: Result<Value, _> = serde_json::from_str(&out.stdout);
        let summary = match parsed {
            Ok(v) => summarize_coverage(&v),
            Err(_) => json!({
                "parse_error": true,
                "raw_stderr": out.stderr,
            }),
        };

        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "coverage": summary,
            "workspace_member": workspace_member,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "workspace_member": {"type": "string"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::write_subproc()
    }
}

/// Formato llvm-cov (camelCase):
/// ```json
/// {
///   "data": [{
///     "totals": {
///       "lines": {"count": N, "covered": M, "percent": P},
///       "functions": {...},
///       "regions": {...}
///     }
///   }]
/// }
/// ```
fn summarize_coverage(v: &Value) -> Value {
    let totals = v
        .get("data")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|entry| entry.get("totals"))
        .cloned()
        .unwrap_or(Value::Null);

    let lines = totals.get("lines").cloned().unwrap_or(Value::Null);
    let functions = totals.get("functions").cloned().unwrap_or(Value::Null);
    let regions = totals.get("regions").cloned().unwrap_or(Value::Null);

    json!({
        "lines": lines,
        "functions": functions,
        "regions": regions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_summarize_coverage() {
        let v = json!({
            "data": [{
                "totals": {
                    "lines": {"count": 1000, "covered": 850, "percent": 85.0},
                    "functions": {"count": 200, "covered": 180, "percent": 90.0},
                    "regions": {"count": 500, "covered": 400, "percent": 80.0}
                }
            }]
        });
        let s = summarize_coverage(&v);
        assert_eq!(s["lines"]["percent"], 85.0);
        assert_eq!(s["functions"]["covered"], 180);
        assert_eq!(s["regions"]["count"], 500);
    }

    #[test]
    fn test_summarize_empty() {
        let s = summarize_coverage(&json!({}));
        assert_eq!(s["lines"], Value::Null);
    }
}
