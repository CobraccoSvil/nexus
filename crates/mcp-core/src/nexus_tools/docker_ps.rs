//! `docker_ps` — lista container del progetto corrente.
//!
//! Filtra per label `com.docker.compose.project=<slug>`.
//! NON espone container `ideai-*` ne' container di altri progetti.

use super::exec;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DockerPsTool;

#[async_trait]
impl NexusToolHandler for DockerPsTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);

        let slug = ctx
            .project_root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| ctx.project_id.to_string());

        let filter = format!("label=com.docker.compose.project={}", slug);

        let mut cmd_args = vec![
            "ps",
            "--filter",
            &filter,
            "--format",
            "{{.ID}}\t{{.Names}}\t{{.Image}}\t{{.Status}}\t{{.Ports}}\t{{.CreatedAt}}",
        ];

        if all {
            cmd_args.insert(1, "-a");
        }

        let out = exec::run_cmd("docker", &cmd_args, &ctx.project_root, 30).await?;

        let mut containers: Vec<Value> = Vec::new();
        for line in out.stdout.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 4 {
                let name = parts.get(1).unwrap_or(&"");
                // Doppio filtro di sicurezza: escludi container ideai-*
                if name.starts_with("ideai-") {
                    continue;
                }
                containers.push(json!({
                    "id": parts.first().unwrap_or(&""),
                    "name": name,
                    "image": parts.get(2).unwrap_or(&""),
                    "status": parts.get(3).unwrap_or(&""),
                    "ports": parts.get(4).unwrap_or(&""),
                    "created": parts.get(5).unwrap_or(&""),
                }));
            }
        }

        Ok(json!({
            "ok": true,
            "project_slug": slug,
            "count": containers.len(),
            "containers": containers,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "all": {
                    "type": "boolean",
                    "description": "Mostra anche container fermi (default: false, solo attivi)"
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
    fn test_safety_readonly() {
        let s = DockerPsTool.safety();
        assert!(s.read_only);
        assert!(!s.can_write_filesystem);
        assert!(s.can_execute_subproc);
    }

    #[test]
    fn test_input_schema_no_required() {
        let s = DockerPsTool.input_schema();
        assert!(s.get("required").is_none());
    }
}
