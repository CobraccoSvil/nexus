//! `docker_build` — costruisce un'immagine Docker dal progetto.
//!
//! Esegue `docker build` con auto-label `com.docker.compose.project=<slug>`.
//! Il Dockerfile deve trovarsi dentro la project_root.

use super::exec;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DockerBuildTool;

/// Verifica che un path sia dentro la project_root (no path traversal).
fn validate_path_in_root(root: &std::path::Path, candidate: &str) -> Result<(), NexusToolError> {
    let full = root.join(candidate);
    let canonical = full.canonicalize().unwrap_or(full);
    if !canonical.starts_with(root) {
        return Err(NexusToolError::BadInput(
            "Path fuori dalla root del progetto. Path traversal non permesso.".into(),
        ));
    }
    Ok(())
}

#[async_trait]
impl NexusToolHandler for DockerBuildTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let tag = args
            .get("tag")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("Parametro 'tag' obbligatorio".into()))?
            .trim()
            .to_string();

        if tag.is_empty() {
            return Err(NexusToolError::BadInput("Tag vuoto".into()));
        }

        let dockerfile = args
            .get("dockerfile")
            .and_then(Value::as_str)
            .unwrap_or("Dockerfile")
            .trim()
            .to_string();

        validate_path_in_root(&ctx.project_root, &dockerfile)?;

        let context_dir = args
            .get("context")
            .and_then(Value::as_str)
            .unwrap_or(".")
            .trim()
            .to_string();

        // Slug del progetto per label
        let slug = ctx
            .project_root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| ctx.project_id.to_string());

        let label = format!("com.docker.compose.project={}", slug);

        let mut cmd_args: Vec<String> = vec![
            "build".to_string(),
            "-f".to_string(),
            dockerfile,
            "-t".to_string(),
            tag.clone(),
            "--label".to_string(),
            label.clone(),
        ];

        // Build args opzionali
        if let Some(build_args) = args.get("build_args").and_then(Value::as_object) {
            for (k, v) in build_args {
                let val = v.as_str().unwrap_or("");
                cmd_args.push("--build-arg".to_string());
                cmd_args.push(format!("{}={}", k, val));
            }
        }

        cmd_args.push(context_dir);

        let args_ref: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();
        let out = exec::run_cmd("docker", &args_ref, &ctx.project_root, ctx.timeout_secs).await?;

        if out.success() {
            Ok(json!({
                "ok": true,
                "tag": tag,
                "label": label,
                "duration_ms": out.duration_ms,
                "output": truncate(&out.stdout, 4000),
            }))
        } else {
            Ok(json!({
                "ok": false,
                "exit_code": out.exit_code,
                "stderr": truncate(&out.stderr, 4000),
                "stdout": truncate(&out.stdout, 2000),
                "duration_ms": out.duration_ms,
            }))
        }
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["tag"],
            "properties": {
                "tag": {
                    "type": "string",
                    "description": "Tag dell'immagine (es. 'myapp:dev', 'myapp:1.0')"
                },
                "dockerfile": {
                    "type": "string",
                    "description": "Percorso relativo al Dockerfile. Default: 'Dockerfile'"
                },
                "context": {
                    "type": "string",
                    "description": "Contesto di build (directory relativa). Default: '.'"
                },
                "build_args": {
                    "type": "object",
                    "description": "Argomenti di build (chiave-valore passati con --build-arg)"
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

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}... [troncato, {} caratteri totali]", &s[..max], s.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety_full() {
        let s = DockerBuildTool.safety();
        assert!(!s.read_only);
        assert!(s.can_write_filesystem);
        assert!(s.can_execute_subproc);
        assert!(s.network_egress);
    }

    #[test]
    fn test_input_schema_requires_tag() {
        let s = DockerBuildTool.input_schema();
        let required = s["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "tag"));
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("abc", 10), "abc");
        let long = "a".repeat(100);
        let t = truncate(&long, 10);
        assert!(t.contains("troncato"));
    }
}
