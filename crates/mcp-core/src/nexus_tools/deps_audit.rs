//! `security::deps_audit` — dispatcher generico per audit delle dipendenze.
//!
//! Strategia:
//! - `Cargo.toml` → `cargo audit --json`
//! - `package.json` → `npm audit --json`
//! - `requirements.txt`|`pyproject.toml` → `pip-audit --format=json`
//!
//! Ritorna sempre un JSON con `{ok, stack, vulnerabilities_count, entries}`.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DepsAuditTool;

fn count_cargo_audit(v: &Value) -> usize {
    v.get("vulnerabilities")
        .and_then(|x| x.get("list"))
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0)
}

fn count_npm_audit(v: &Value) -> usize {
    v.get("metadata")
        .and_then(|m| m.get("vulnerabilities"))
        .and_then(|o| o.get("total"))
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .or_else(|| {
            // Formato legacy: top-level "actions" o "advisories"
            v.get("vulnerabilities")
                .and_then(Value::as_object)
                .map(|m| m.len())
        })
        .unwrap_or(0)
}

fn count_pip_audit(v: &Value) -> usize {
    v.get("dependencies")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|d| {
                    d.get("vulns")
                        .and_then(Value::as_array)
                        .map(|v| v.len())
                        .unwrap_or(0)
                })
                .sum()
        })
        .or_else(|| v.as_array().map(|a| a.len()))
        .unwrap_or(0)
}

#[async_trait]
impl NexusToolHandler for DepsAuditTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let root = &ctx.project_root;
        if root.join("Cargo.toml").is_file() {
            let out = run_cmd(
                "cargo",
                &["audit", "--json"],
                root,
                ctx.timeout_secs,
            )
            .await?;
            let parsed: Value = serde_json::from_str(&out.stdout).unwrap_or(Value::Null);
            let count = count_cargo_audit(&parsed);
            return Ok(json!({
                "ok": out.success() || count == 0,
                "stack": "rust",
                "exit_code": out.exit_code,
                "duration_ms": out.duration_ms,
                "vulnerabilities_count": count,
                "report": parsed,
            }));
        }
        if root.join("package.json").is_file() {
            let out = run_cmd("npm", &["audit", "--json"], root, ctx.timeout_secs).await?;
            let parsed: Value = serde_json::from_str(&out.stdout).unwrap_or(Value::Null);
            let count = count_npm_audit(&parsed);
            return Ok(json!({
                "ok": count == 0,
                "stack": "node",
                "exit_code": out.exit_code,
                "duration_ms": out.duration_ms,
                "vulnerabilities_count": count,
                "report": parsed,
            }));
        }
        if root.join("requirements.txt").is_file() || root.join("pyproject.toml").is_file() {
            let out = run_cmd(
                "pip-audit",
                &["--format=json"],
                root,
                ctx.timeout_secs,
            )
            .await?;
            let parsed: Value = serde_json::from_str(&out.stdout).unwrap_or(Value::Null);
            let count = count_pip_audit(&parsed);
            return Ok(json!({
                "ok": count == 0,
                "stack": "python",
                "exit_code": out.exit_code,
                "duration_ms": out.duration_ms,
                "vulnerabilities_count": count,
                "report": parsed,
            }));
        }
        Ok(json!({
            "ok": false,
            "stack": "unknown",
            "error": "No supported manifest found for dependency audit",
        }))
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: true,
            can_write_filesystem: false,
            can_execute_subproc: true,
            network_egress: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_cargo_audit() {
        let v = json!({"vulnerabilities": {"list": [{"advisory": {}}, {"advisory": {}}]}});
        assert_eq!(count_cargo_audit(&v), 2);
    }

    #[test]
    fn test_count_npm_audit_v7() {
        let v = json!({"metadata": {"vulnerabilities": {"total": 5}}});
        assert_eq!(count_npm_audit(&v), 5);
    }

    #[test]
    fn test_count_pip_audit() {
        let v = json!({"dependencies": [{"vulns": [{"id":"X"}]}, {"vulns": []}]});
        assert_eq!(count_pip_audit(&v), 1);
    }

    #[test]
    fn test_safety_has_network_egress() {
        assert!(DepsAuditTool.safety().network_egress);
    }
}
