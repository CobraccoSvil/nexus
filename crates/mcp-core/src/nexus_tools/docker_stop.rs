//! `docker_stop` — ferma un singolo container del progetto.
//!
//! Verifica che il container abbia label del progetto corrente PRIMA
//! di eseguire lo stop. Container `ideai-*` sempre rifiutati.

use super::exec;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DockerStopTool;

const PROTECTED_PREFIX: &str = "ideai-";

fn validate_not_protected(name: &str) -> Result<(), NexusToolError> {
    if name.starts_with(PROTECTED_PREFIX) {
        return Err(NexusToolError::BadInput(format!(
            "Container '{}' e' infrastruttura Nexus. VIETATO fermarlo da agenti progetto.",
            name
        )));
    }
    Ok(())
}

async fn verify_container_label(
    name: &str,
    slug: &str,
    project_root: &std::path::Path,
) -> Result<(), NexusToolError> {
    let out = exec::run_cmd(
        "docker",
        &[
            "inspect",
            "--format",
            "{{index .Config.Labels \"com.docker.compose.project\"}}",
            name,
        ],
        project_root,
        10,
    )
    .await?;

    let container_slug = out.stdout.trim();
    if container_slug != slug {
        return Err(NexusToolError::BadInput(format!(
            "Container '{}' non appartiene al progetto corrente (label='{}', atteso='{}'). Stop negato.",
            name, container_slug, slug
        )));
    }
    Ok(())
}

#[async_trait]
impl NexusToolHandler for DockerStopTool {
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

        let timeout_str = args
            .get("timeout")
            .and_then(Value::as_u64)
            .unwrap_or(10)
            .to_string();

        let out = exec::run_cmd(
            "docker",
            &["stop", "-t", &timeout_str, &container],
            &ctx.project_root,
            ctx.timeout_secs,
        )
        .await?;

        if out.success() {
            Ok(json!({
                "ok": true,
                "container": container,
                "message": format!("Container '{}' fermato", container),
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
                    "description": "Nome esatto del container da fermare"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Secondi da attendere prima di SIGKILL (default: 10)"
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
    fn test_validate_not_protected_blocks_ideai() {
        assert!(validate_not_protected("ideai-postgres-nexus-1").is_err());
        assert!(validate_not_protected("ideai-redis-1").is_err());
        assert!(validate_not_protected("ideai-grafana-1").is_err());
    }

    #[test]
    fn test_validate_not_protected_allows_project() {
        assert!(validate_not_protected("myapp-web-1").is_ok());
        assert!(validate_not_protected("redemptor-backend").is_ok());
    }

    #[test]
    fn test_safety() {
        let s = DockerStopTool.safety();
        assert!(!s.read_only);
        assert!(!s.can_write_filesystem);
        assert!(s.can_execute_subproc);
    }

    #[test]
    fn test_input_requires_container() {
        let s = DockerStopTool.input_schema();
        let required = s["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "container"));
    }
}
