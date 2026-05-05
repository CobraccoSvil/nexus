//! `dependencies::cargo_dep_versions` — flat dep version table from `cargo metadata`.
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;

pub struct CargoDepVersionsTool;

#[async_trait]
impl NexusToolHandler for CargoDepVersionsTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let out = run_cmd("cargo", &["metadata", "--format-version=1"], &ctx.project_root, ctx.timeout_secs).await?;
        if !out.success() {
            return Err(NexusToolError::Exec { exit_code: out.exit_code, stderr: out.stderr });
        }
        let parsed: Value = serde_json::from_str(&out.stdout).unwrap_or_else(|_| json!({}));
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        if let Some(pkgs) = parsed.get("packages").and_then(Value::as_array) {
            for p in pkgs {
                if let (Some(n), Some(v)) = (p.get("name").and_then(Value::as_str), p.get("version").and_then(Value::as_str)) {
                    map.entry(n.to_string()).or_default().push(v.to_string());
                }
            }
        }
        let total_pkgs = map.len();
        let mut multi: Vec<Value> = map.iter()
            .filter(|(_, v)| v.len() > 1)
            .map(|(k, v)| json!({"name": k, "versions": v}))
            .collect();
        multi.sort_by(|a, b| a["name"].as_str().unwrap_or("").cmp(b["name"].as_str().unwrap_or("")));
        Ok(json!({
            "ok": true,
            "total_unique_packages": total_pkgs,
            "duplicate_packages": multi.len(),
            "duplicates": multi,
            "duration_ms": out.duration_ms,
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only_subproc() }
}
