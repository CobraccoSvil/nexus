//! `docker_rm` — rimuove un singolo container fermo del progetto.
//!
//! Verifica label progetto PRIMA di rimuovere. Container `ideai-*` sempre rifiutati.
//! Il container deve essere fermo (non forza rimozione di container attivi senza `force`).

use super::exec;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DockerRmTool;

// validate_not_protected + verify_container_label: punto unico in
// nexus_tools::docker_helpers (regola L / ADR 0026, step S31).
use super::docker_helpers::{validate_not_protected, verify_container_label};

#[async_trait]
impl NexusToolHandler for DockerRmTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let container = args
            .get("container")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("Parametro 'container' obbligatorio".into()))?
            .trim()
            .to_string();

        if container.is_empty() {
            return Err(NexusToolError::BadInput("Nome container vuoto".into()));
        }

        validate_not_protected(&container)?;

        let slug = ctx
            .project_root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| ctx.project_id.to_string());

        verify_container_label(&container, &slug, &ctx.project_root).await?;

        let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
        let volumes = args
            .get("volumes")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let mut cmd_args = vec!["rm"];
        if force {
            cmd_args.push("-f");
        }
        if volumes {
            cmd_args.push("-v");
        }
        cmd_args.push(&container);

        let out = exec::run_cmd("docker", &cmd_args, &ctx.project_root, 30).await?;

        if out.success() {
            Ok(json!({
                "ok": true,
                "container": container,
                "message": format!("Container '{}' rimosso", container),
            }))
        } else {
            Ok(json!({
                "ok": false,
                "container": container,
                "exit_code": out.exit_code,
                "stderr": out.stderr.chars().take(2000).collect::<String>(),
            }))
        }
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["container"],
            "properties": {
                "container": {
                    "type": "string",
                    "description": "Nome esatto del container da rimuovere"
                },
                "force": {
                    "type": "boolean",
                    "description": "Forza rimozione anche se attivo (default: false)"
                },
                "volumes": {
                    "type": "boolean",
                    "description": "Rimuovi anche i volumi anonimi associati (default: false)"
                }
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: false,
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
    fn test_blocks_ideai_containers() {
        assert!(validate_not_protected("ideai-postgres-nexus-1").is_err());
        assert!(validate_not_protected("ideai-qdrant-1").is_err());
    }

    #[test]
    fn test_allows_project_containers() {
        assert!(validate_not_protected("myapp-db-1").is_ok());
        assert!(validate_not_protected("app-worker").is_ok());
    }

    #[test]
    fn test_safety() {
        let s = DockerRmTool.safety();
        assert!(!s.read_only);
        assert!(s.can_execute_subproc);
    }

    #[test]
    fn test_input_requires_container() {
        let s = DockerRmTool.input_schema();
        let required = s["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "container"));
    }
}
