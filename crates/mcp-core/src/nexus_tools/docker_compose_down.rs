//! `docker_compose_down` — ferma e rimuove servizi compose del progetto.
//!
//! OBBLIGATORIO specificare il path del file compose (`-f <path>`).
//! Mai compose globali. Il file deve trovarsi dentro la project_root.

use super::exec;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DockerComposeDownTool;

fn validate_compose_path(root: &std::path::Path, compose_file: &str) -> Result<String, NexusToolError> {
    if compose_file.is_empty() {
        return Err(NexusToolError::BadInput(
            "Parametro 'compose_file' obbligatorio. Non e' permesso usare compose globali.".into(),
        ));
    }

    let full = root.join(compose_file);
    let canonical = full.canonicalize().map_err(|_| {
        NexusToolError::BadInput(format!("File compose '{}' non trovato", compose_file))
    })?;

    if !canonical.starts_with(root) {
        return Err(NexusToolError::BadInput(
            "File compose fuori dalla root del progetto. Path traversal non permesso.".into(),
        ));
    }

    Ok(canonical.to_string_lossy().to_string())
}

#[async_trait]
impl NexusToolHandler for DockerComposeDownTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let compose_file = args
            .get("compose_file")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("Parametro 'compose_file' obbligatorio".into()))?
            .trim()
            .to_string();

        let abs_compose = validate_compose_path(&ctx.project_root, &compose_file)?;

        let remove_volumes = args.get("volumes").and_then(Value::as_bool).unwrap_or(false);
        let remove_images = args.get("rmi").and_then(Value::as_str).map(|s| s.to_string());

        let mut cmd_args: Vec<String> = vec![
            "compose".to_string(),
            "-f".to_string(),
            abs_compose,
            "down".to_string(),
        ];

        if remove_volumes {
            cmd_args.push("-v".to_string());
        }

        if let Some(rmi) = &remove_images {
            cmd_args.push("--rmi".to_string());
            cmd_args.push(rmi.clone());
        }

        let args_ref: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();
        let out = exec::run_cmd("docker", &args_ref, &ctx.project_root, ctx.timeout_secs).await?;

        if out.success() {
            Ok(json!({
                "ok": true,
                "compose_file": compose_file,
                "volumes_removed": remove_volumes,
                "duration_ms": out.duration_ms,
                "output": if out.stderr.len() > 4000 {
                    format!("{}... [troncato]", &out.stderr[..4000])
                } else {
                    out.stderr.clone()
                },
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
            "required": ["compose_file"],
            "properties": {
                "compose_file": {
                    "type": "string",
                    "description": "Percorso relativo al file docker-compose.yml dentro la root del progetto. OBBLIGATORIO."
                },
                "volumes": {
                    "type": "boolean",
                    "description": "Rimuovi anche i volumi associati (default: false)"
                },
                "rmi": {
                    "type": "string",
                    "enum": ["all", "local"],
                    "description": "Rimuovi immagini: 'all' (tutte) o 'local' (solo quelle senza tag custom)"
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
    use std::path::PathBuf;

    #[test]
    fn test_validate_compose_empty() {
        let root = PathBuf::from("/tmp/project");
        assert!(validate_compose_path(&root, "").is_err());
    }

    #[test]
    fn test_safety() {
        let s = DockerComposeDownTool.safety();
        assert!(!s.read_only);
        assert!(!s.can_write_filesystem);
        assert!(s.can_execute_subproc);
    }

    #[test]
    fn test_input_requires_compose_file() {
        let s = DockerComposeDownTool.input_schema();
        let required = s["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "compose_file"));
    }
}
