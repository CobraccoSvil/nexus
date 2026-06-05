//! `docker_logs` — legge i log di un container del progetto.
//!
//! Verifica che il container abbia label del progetto corrente.
//! Non puo' leggere log di container `ideai-*`.

use super::exec;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DockerLogsTool;

// Helper: punto unico in docker_helpers (regola L, S79).
use super::docker_helpers::{extract_container_and_slug, fetch_container_compose_project};

const PROTECTED_PREFIX: &str = "ideai-";

fn validate_not_protected(name: &str) -> Result<(), NexusToolError> {
    if name.starts_with(PROTECTED_PREFIX) {
        return Err(NexusToolError::BadInput(format!(
            "Container '{}' appartiene all'infrastruttura Nexus. Accesso negato.",
            name
        )));
    }
    Ok(())
}

/// Verifica che il container abbia la label del progetto corrente (msg verbose
/// con label='...' atteso='...' per il debug dei log).
async fn verify_container_label(
    name: &str,
    slug: &str,
    project_root: &std::path::Path,
) -> Result<(), NexusToolError> {
    let container_slug = fetch_container_compose_project(name, project_root).await?;
    if container_slug != slug {
        return Err(NexusToolError::BadInput(format!(
            "Container '{}' non appartiene al progetto corrente (label='{}', atteso='{}')",
            name, container_slug, slug
        )));
    }
    Ok(())
}

#[async_trait]
impl NexusToolHandler for DockerLogsTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        // Punto unico extract_container_and_slug (regola L, S79).
        let (container, slug) = extract_container_and_slug(ctx, args)?;
        validate_not_protected(&container)?;
        verify_container_label(&container, &slug, &ctx.project_root).await?;

        let tail = args.get("tail").and_then(Value::as_u64).unwrap_or(100);

        let tail_str = tail.to_string();
        let mut cmd_args = vec!["logs", "--tail", &tail_str];

        let timestamps = args
            .get("timestamps")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if timestamps {
            cmd_args.push("--timestamps");
        }

        cmd_args.push(&container);

        let out = exec::run_cmd("docker", &cmd_args, &ctx.project_root, 30).await?;

        // Docker logs scrive su stderr per alcuni container
        let logs = if out.stdout.is_empty() {
            &out.stderr
        } else {
            &out.stdout
        };

        let truncated = if logs.len() > 8000 {
            format!(
                "{}... [troncato, {} caratteri totali]",
                &logs[..8000],
                logs.len()
            )
        } else {
            logs.to_string()
        };

        Ok(json!({
            "ok": true,
            "container": container,
            "tail": tail,
            "logs": truncated,
            "lines": truncated.lines().count(),
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["container"],
            "properties": {
                "container": {
                    "type": "string",
                    "description": "Nome o ID del container"
                },
                "tail": {
                    "type": "integer",
                    "description": "Numero di righe dalla fine (default: 100)"
                },
                "timestamps": {
                    "type": "boolean",
                    "description": "Mostra timestamp per ogni riga (default: false)"
                }
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: true,
            can_write_filesystem: false,
            can_execute_subproc: true,
            network_egress: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_not_protected() {
        assert!(validate_not_protected("ideai-postgres-nexus-1").is_err());
        assert!(validate_not_protected("ideai-qdrant-1").is_err());
        assert!(validate_not_protected("myapp-web").is_ok());
    }

    #[test]
    fn test_safety_readonly() {
        let s = DockerLogsTool.safety();
        assert!(s.read_only);
        assert!(s.can_execute_subproc);
    }

    #[test]
    fn test_input_schema_requires_container() {
        let s = DockerLogsTool.input_schema();
        let required = s["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "container"));
    }
}
