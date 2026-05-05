//! `dependencies::cargo_update` — wrapper di `cargo update [-p CRATE]`.
//!
//! Aggiorna `Cargo.lock` alle ultime versioni compatibili con `Cargo.toml`.
//! Richiede accesso di rete verso crates.io (o registry privato) e scrive
//! su `Cargo.lock`, quindi safety = write_subproc con network_egress=true.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoUpdateTool;

#[async_trait]
impl NexusToolHandler for CargoUpdateTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let crate_name = args
            .get("crate")
            .and_then(Value::as_str)
            .map(String::from);
        let dry_run = args
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let aggressive = args
            .get("aggressive")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let mut cmd: Vec<String> = vec!["update".into()];
        if dry_run {
            cmd.push("--dry-run".into());
        }
        if aggressive {
            cmd.push("--aggressive".into());
        }
        if let Some(c) = &crate_name {
            cmd.push("-p".into());
            cmd.push(c.clone());
        }
        let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        let out = run_cmd("cargo", &refs, &ctx.project_root, ctx.timeout_secs).await?;

        // cargo update stampa su stderr le righe "Updating ..." e "Adding ..."
        // Le contiamo per dare feedback numerico.
        let stream = if out.stderr.is_empty() {
            &out.stdout
        } else {
            &out.stderr
        };
        let updated_count = stream
            .lines()
            .filter(|l| l.trim_start().starts_with("Updating "))
            .count();
        let added_count = stream
            .lines()
            .filter(|l| l.trim_start().starts_with("Adding "))
            .count();
        let removed_count = stream
            .lines()
            .filter(|l| l.trim_start().starts_with("Removing "))
            .count();

        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "updated": updated_count,
            "added": added_count,
            "removed": removed_count,
            "dry_run": dry_run,
            "crate": crate_name,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "crate": {"type": "string", "description": "Crate specifico da aggiornare (opzionale)"},
                "dry_run": {"type": "boolean"},
                "aggressive": {"type": "boolean"}
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety_network_egress() {
        let s = CargoUpdateTool.safety();
        assert!(s.network_egress);
        assert!(s.can_write_filesystem);
    }
}
