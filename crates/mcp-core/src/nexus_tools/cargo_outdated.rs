//! `dependencies::cargo_outdated` — wrapper di `cargo outdated --format json`.
//!
//! Richiede `cargo install cargo-outdated`. Ritorna la lista dei crate che
//! hanno versioni più recenti disponibili, categorizzati per "level"
//! (project vs workspace) e se il bump è compatibile o richiede major.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoOutdatedTool;

#[async_trait]
impl NexusToolHandler for CargoOutdatedTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let workspace_member = args
            .get("workspace_member")
            .and_then(Value::as_str)
            .map(String::from);
        let workspace_wide = args
            .get("workspace")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let mut cmd: Vec<String> = vec!["outdated".into(), "--format".into(), "json".into()];
        if workspace_wide {
            cmd.push("--workspace".into());
        }
        if let Some(m) = &workspace_member {
            cmd.push("-p".into());
            cmd.push(m.clone());
        }
        let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        let out = run_cmd("cargo", &refs, &ctx.project_root, ctx.timeout_secs).await?;

        let summary = parse_outdated_output(&out.stdout);

        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "total_outdated": summary.total,
            "compat_bumps": summary.compat,
            "incompat_bumps": summary.incompat,
            "dependencies": summary.deps,
            "workspace_member": workspace_member,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "workspace_member": {"type": "string"},
                "workspace": {"type": "boolean", "description": "Analizza l'intero workspace"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: false,
            can_write_filesystem: true,
            can_execute_subproc: true,
            network_egress: true,
        }
    }
}

#[derive(Default)]
struct OutdatedSummary {
    total: usize,
    compat: usize,
    incompat: usize,
    deps: Vec<Value>,
}

/// cargo-outdated emette un JSON con campo `dependencies[]`. Ogni entry ha
/// `name`, `project` (versione corrente), `compat` (ultima compatibile),
/// `latest` (ultima in assoluto), `kind`, ecc.
fn parse_outdated_output(stdout: &str) -> OutdatedSummary {
    let mut summary = OutdatedSummary::default();
    let parsed: Value = match serde_json::from_str(stdout.trim()) {
        Ok(v) => v,
        Err(_) => return summary,
    };

    let deps = parsed
        .get("dependencies")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for dep in &deps {
        summary.total += 1;
        let project = dep.get("project").and_then(Value::as_str).unwrap_or("");
        let compat = dep.get("compat").and_then(Value::as_str).unwrap_or("");
        let latest = dep.get("latest").and_then(Value::as_str).unwrap_or("");
        if !compat.is_empty() && compat != project {
            summary.compat += 1;
        }
        if !latest.is_empty() && latest != project && latest != compat {
            summary.incompat += 1;
        }
        summary.deps.push(json!({
            "name": dep.get("name").cloned().unwrap_or(Value::Null),
            "project": project,
            "compat": compat,
            "latest": latest,
            "kind": dep.get("kind").cloned().unwrap_or(Value::Null),
        }));
    }

    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_outdated_basic() {
        let stdout = r#"{"dependencies":[{"name":"serde","project":"1.0.100","compat":"1.0.190","latest":"2.0.0","kind":null}]}"#;
        let s = parse_outdated_output(stdout);
        assert_eq!(s.total, 1);
        assert_eq!(s.compat, 1);
        assert_eq!(s.incompat, 1);
    }

    #[test]
    fn test_parse_empty() {
        let s = parse_outdated_output(r#"{"dependencies":[]}"#);
        assert_eq!(s.total, 0);
    }
}
