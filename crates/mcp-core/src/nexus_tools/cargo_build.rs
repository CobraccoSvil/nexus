//! `build::cargo_build` — wrapper di `cargo build [--release] [-p MEMBER]`.
//!
//! Lancia una compilazione completa del progetto Rust. A differenza di
//! cargo_check, produce artefatti in `target/` (.rlib, .exe). Ritorna
//! durata totale, exit code, stream di errori/warning estratti dal flusso
//! NDJSON (come per cargo_check).

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoBuildTool;

#[async_trait]
impl NexusToolHandler for CargoBuildTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let workspace_member = args
            .get("workspace_member")
            .and_then(Value::as_str)
            .map(String::from);
        let release = args
            .get("release")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let all_targets = args
            .get("all_targets")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let mut cmd: Vec<String> = vec!["build".into(), "--message-format=json".into()];
        if release {
            cmd.push("--release".into());
        }
        if all_targets {
            cmd.push("--all-targets".into());
        }
        if let Some(m) = &workspace_member {
            cmd.push("-p".into());
            cmd.push(m.clone());
        }
        let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();

        let out = run_cmd("cargo", &refs, &ctx.project_root, ctx.timeout_secs).await?;

        let (errors, warnings) = super::parse_ndjson::extract_cargo_diagnostics(&out.stdout);

        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "errors": errors,
            "warnings": warnings,
            "workspace_member": workspace_member,
            "release": release,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "workspace_member": {"type": "string"},
                "release": {"type": "boolean"},
                "all_targets": {"type": "boolean"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::write_subproc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety_write_subproc() {
        let s = CargoBuildTool.safety();
        assert!(s.can_write_filesystem && s.can_execute_subproc);
        assert!(!s.read_only);
    }
}
