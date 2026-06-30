//! `build::cargo_clean` — wrapper di `cargo clean [-p MEMBER]`.
//!
//! Rimuove la directory `target/` (o solo gli artefatti di un package se
//! `workspace_member` è specificato). È destructive sul filesystem e
//! dichiarato come tale nel safety flag.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoCleanTool;

#[async_trait]
impl NexusToolHandler for CargoCleanTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let workspace_member = args
            .get("workspace_member")
            .and_then(Value::as_str)
            .map(String::from);
        let release = args
            .get("release")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let mut cmd: Vec<String> = vec!["clean".into()];
        if release {
            cmd.push("--release".into());
        }
        if let Some(m) = &workspace_member {
            cmd.push("-p".into());
            cmd.push(m.clone());
        }
        let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        let out = run_cmd("cargo", &refs, &ctx.project_root, ctx.timeout_secs).await?;

        if !out.success() {
            return Err(NexusToolError::Exec {
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }

        Ok(json!({
            "ok": true,
            "duration_ms": out.duration_ms,
            "workspace_member": workspace_member,
            "release": release,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "workspace_member": {"type": "string"},
                "release": {"type": "boolean"}
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
    fn test_safety_writes_fs() {
        let s = CargoCleanTool.safety();
        assert!(s.can_write_filesystem);
        assert!(!s.read_only);
    }
}
