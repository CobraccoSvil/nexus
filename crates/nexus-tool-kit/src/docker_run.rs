//! `docker_run` — esegue un container Docker con safety.
//!
//! Forza label progetto. Vieta `--privileged` e `--net=host`.
//! Container `ideai-*` non possono essere usati come nome.

use super::exec;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DockerRunTool;

/// Nomi riservati all'infrastruttura Nexus. Mai usabili come nome container.
const PROTECTED_PREFIX: &str = "ideai-";

fn validate_container_name(name: &str) -> Result<(), NexusToolError> {
    if name.starts_with(PROTECTED_PREFIX) {
        return Err(NexusToolError::BadInput(format!(
            "Nome container '{}' riservato all'infrastruttura Nexus. Non utilizzabile.",
            name
        )));
    }
    Ok(())
}

#[async_trait]
impl NexusToolHandler for DockerRunTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let image = args
            .get("image")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("Parametro 'image' obbligatorio".into()))?
            .trim()
            .to_string();

        if image.is_empty() {
            return Err(NexusToolError::BadInput("Immagine vuota".into()));
        }

        let name = args
            .get("name")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string());
        if let Some(ref n) = name {
            validate_container_name(n)?;
        }

        // ── PR hardening: quota container per progetto ─────────────────────
        // get_pool ritorna un pool short-lived verso Nexus (riusa env DATABASE_URL).
        // Best-effort: se il pool fallisce, salta il check (degrado graceful).
        if let Ok(nexus_pool) = super::db_helper::get_pool().await {
            // Separazione DB: agent_processes vive nel DB del progetto; il pool
            // si risolve via nexus-project-pools (punto unico, regola L). Non
            // risolvibile -> WARN + skip check, stesso degrado del pool meta.
            // Il run_pool NON va chiuso: e' condiviso dalla cache del crate.
            let run_pool =
                match nexus_project_pools::project_data_pool(&nexus_pool, ctx.project_id).await {
                    Ok(p) => Some(p),
                    Err(e) => {
                        tracing::warn!(
                            project_id = %ctx.project_id,
                            error = %e,
                            "docker_run: pool dominio run non risolvibile, quota container non applicata"
                        );
                        None
                    }
                };
            if let Some(run_pool) = run_pool {
                if let Err(reason) =
                    crate::quotas::check_can_start_container(&nexus_pool, &run_pool, ctx.project_id)
                        .await
                {
                    crate::audit::record_audit(
                        crate::audit::AuditEntry::blocked(
                            ctx.project_id,
                            "container_create",
                            "container",
                        )
                        .with_resource(image.clone())
                        .with_details(json!({"reason": reason, "name": name})),
                    );
                    nexus_pool.close().await;
                    return Err(NexusToolError::BadInput(format!(
                        "Quota container raggiunta: {}",
                        reason
                    )));
                }
            }
            nexus_pool.close().await;
        }

        let detach = args.get("detach").and_then(Value::as_bool).unwrap_or(true);

        // Slug progetto per label
        let slug = ctx
            .project_root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| ctx.project_id.to_string());

        let label = format!("com.docker.compose.project={}", slug);

        let mut cmd_args: Vec<String> = vec!["run".to_string()];

        if detach {
            cmd_args.push("-d".to_string());
        }

        cmd_args.push("--label".to_string());
        cmd_args.push(label.clone());

        // PR hardening: label aggiuntiva nexus.project_id permette al port_enforcer
        // di associare in modo univoco container al progetto.
        cmd_args.push("--label".to_string());
        cmd_args.push(format!("nexus.project_id={}", ctx.project_id));

        if let Some(ref n) = name {
            cmd_args.push("--name".to_string());
            cmd_args.push(n.clone());
        }

        // Porte
        if let Some(ports) = args.get("ports").and_then(Value::as_array) {
            for p in ports {
                if let Some(ps) = p.as_str() {
                    cmd_args.push("-p".to_string());
                    cmd_args.push(ps.to_string());
                }
            }
        }

        // Variabili d'ambiente
        if let Some(env) = args.get("env").and_then(Value::as_object) {
            for (k, v) in env {
                let val = v.as_str().unwrap_or("");
                cmd_args.push("-e".to_string());
                cmd_args.push(format!("{}={}", k, val));
            }
        }

        // Volumi
        if let Some(volumes) = args.get("volumes").and_then(Value::as_array) {
            for vol in volumes {
                if let Some(vs) = vol.as_str() {
                    cmd_args.push("-v".to_string());
                    cmd_args.push(vs.to_string());
                }
            }
        }

        cmd_args.push(image.clone());

        // Comando opzionale
        if let Some(command) = args.get("command").and_then(Value::as_str) {
            for part in command.split_whitespace() {
                cmd_args.push(part.to_string());
            }
        }

        let args_ref: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();
        let out = exec::run_cmd("docker", &args_ref, &ctx.project_root, ctx.timeout_secs).await?;

        if out.success() {
            let container_id = out.stdout.trim().to_string();
            crate::audit::record_audit(
                crate::audit::AuditEntry::allowed(
                    ctx.project_id,
                    "container_create",
                    "container",
                )
                .with_resource(container_id.clone())
                .with_details(json!({"image": image, "name": name, "detach": detach})),
            );
            Ok(json!({
                "ok": true,
                "container_id": container_id,
                "image": image,
                "name": name,
                "label": label,
                "detach": detach,
            }))
        } else {
            Ok(json!({
                "ok": false,
                "exit_code": out.exit_code,
                "stderr": out.stderr.chars().take(4000).collect::<String>(),
            }))
        }
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["image"],
            "properties": {
                "image": {
                    "type": "string",
                    "description": "Nome dell'immagine Docker (es. 'myapp:dev', 'postgres:16')"
                },
                "name": {
                    "type": "string",
                    "description": "Nome del container. Non puo' iniziare con 'ideai-' (riservato)."
                },
                "detach": {
                    "type": "boolean",
                    "description": "Esegui in background (default: true)"
                },
                "ports": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Mapping porte (es. ['8080:80', '5432:5432'])"
                },
                "env": {
                    "type": "object",
                    "description": "Variabili d'ambiente (chiave-valore)"
                },
                "volumes": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Volumi da montare (es. ['./data:/data'])"
                },
                "command": {
                    "type": "string",
                    "description": "Comando da eseguire nel container"
                }
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
    fn test_validate_container_name_protected() {
        assert!(validate_container_name("ideai-postgres-nexus-1").is_err());
        assert!(validate_container_name("ideai-qdrant-1").is_err());
        assert!(validate_container_name("ideai-redis-1").is_err());
    }

    #[test]
    fn test_validate_container_name_ok() {
        assert!(validate_container_name("myapp-backend").is_ok());
        assert!(validate_container_name("redemptor-web-1").is_ok());
    }

    #[test]
    fn test_input_schema_requires_image() {
        let s = DockerRunTool.input_schema();
        let required = s["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "image"));
    }

    #[test]
    fn test_safety() {
        let s = DockerRunTool.safety();
        assert!(!s.read_only);
        assert!(s.can_write_filesystem);
        assert!(s.can_execute_subproc);
    }
}
