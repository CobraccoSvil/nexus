//! `testing::coverage_report` — alias generico per test coverage.
//!
//! Strategia:
//! - `Cargo.toml` → `cargo llvm-cov --json --summary-only` (riuso logica di
//!   `TestCoverageTool`)
//! - `package.json` con script `coverage` → `npm run coverage`
//! - Altri stack → errore strutturato
//!
//! Differisce da `test_coverage` per essere un dispatcher multi-linguaggio.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CoverageReportTool;

fn summarize_llvm_cov(v: &Value) -> Value {
    let totals = v
        .get("data")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|entry| entry.get("totals"))
        .cloned()
        .unwrap_or(Value::Null);
    json!({
        "lines": totals.get("lines").cloned().unwrap_or(Value::Null),
        "functions": totals.get("functions").cloned().unwrap_or(Value::Null),
        "regions": totals.get("regions").cloned().unwrap_or(Value::Null),
    })
}

#[async_trait]
impl NexusToolHandler for CoverageReportTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let cargo_toml = ctx.project_root.join("Cargo.toml");
        let package_json = ctx.project_root.join("package.json");

        if cargo_toml.is_file() {
            let out = run_cmd(
                "cargo",
                &["llvm-cov", "--json", "--summary-only"],
                &ctx.project_root,
                ctx.timeout_secs,
            )
            .await?;
            let summary = serde_json::from_str::<Value>(&out.stdout)
                .map(|v| summarize_llvm_cov(&v))
                .unwrap_or_else(|_| json!({"parse_error": true}));
            return Ok(json!({
                "ok": out.success(),
                "stack": "rust",
                "exit_code": out.exit_code,
                "duration_ms": out.duration_ms,
                "coverage": summary,
            }));
        }

        if package_json.is_file() {
            let content = std::fs::read_to_string(&package_json).unwrap_or_default();
            let v: Value = serde_json::from_str(&content).unwrap_or(Value::Null);
            let has_cov = v
                .get("scripts")
                .and_then(|s| s.get("coverage"))
                .and_then(Value::as_str)
                .is_some();
            if !has_cov {
                return Ok(json!({
                    "ok": false,
                    "stack": "node",
                    "error": "package.json has no 'coverage' script defined",
                }));
            }
            // Punto unico in nexus_tools::run_npm_script_node_stack (regola L, S65).
            return super::run_npm_script_node_stack(ctx, "coverage").await;
        }

        Ok(json!({
            "ok": false,
            "stack": "unknown",
            "error": "No supported project manifest found (Cargo.toml, package.json)",
        }))
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::write_subproc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_summarize_llvm_cov() {
        let v = json!({
            "data": [{
                "totals": {
                    "lines": {"percent": 75.5},
                    "functions": {"percent": 80.0},
                    "regions": {"percent": 70.0}
                }
            }]
        });
        let s = summarize_llvm_cov(&v);
        assert_eq!(s["lines"]["percent"], 75.5);
    }

    #[test]
    fn test_safety_writes() {
        assert!(CoverageReportTool.safety().can_write_filesystem);
    }
}
