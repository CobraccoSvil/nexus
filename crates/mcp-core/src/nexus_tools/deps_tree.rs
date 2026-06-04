//! `dependencies::deps_tree` — dispatcher generico che mostra l'albero delle
//! dipendenze del progetto rilevando lo stack dal filesystem.
//!
//! Strategia:
//! - `Cargo.toml` → `cargo tree` (riusa CargoTreeTool-like logica minimale)
//! - `package.json` → `npm list --json --depth=<depth>`
//! - `requirements.txt`|`pyproject.toml` → `pipdeptree --json`
//!
//! Se nessuno di questi file è presente, ritorna un errore strutturato.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DepsTreeTool;

fn detect_stack(root: &std::path::Path) -> Option<&'static str> {
    if root.join("Cargo.toml").is_file() {
        Some("rust")
    } else if root.join("package.json").is_file() {
        Some("node")
    } else if root.join("pyproject.toml").is_file() || root.join("requirements.txt").is_file() {
        Some("python")
    } else {
        None
    }
}

#[async_trait]
impl NexusToolHandler for DepsTreeTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let depth = args.get("depth").and_then(Value::as_u64);
        let stack = match detect_stack(&ctx.project_root) {
            Some(s) => s,
            None => {
                return Ok(json!({
                    "ok": false,
                    "error": "No supported manifest (Cargo.toml, package.json, pyproject.toml, requirements.txt) found in project root",
                    "stack": "unknown",
                }));
            }
        };

        match stack {
            "rust" => {
                let mut cmd_args: Vec<String> = vec!["tree".into()];
                if let Some(d) = depth {
                    cmd_args.push("--depth".into());
                    cmd_args.push(d.to_string());
                }
                let refs: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();
                let out = run_cmd("cargo", &refs, &ctx.project_root, ctx.timeout_secs).await?;
                Ok(json!({
                    "ok": out.success(),
                    "stack": "rust",
                    "exit_code": out.exit_code,
                    "duration_ms": out.duration_ms,
                    "tree": out.stdout,
                    "error": if out.success() { Value::Null } else { json!(out.stderr) },
                }))
            }
            "node" => {
                let depth_str = depth.unwrap_or(1).to_string();
                let args = ["list", "--json", "--depth", &depth_str];
                let out = run_cmd("npm", &args, &ctx.project_root, ctx.timeout_secs).await?;
                let parsed: Value = serde_json::from_str(&out.stdout)
                    .unwrap_or_else(|_| json!({"raw": out.stdout}));
                Ok(json!({
                    "ok": out.success(),
                    "stack": "node",
                    "exit_code": out.exit_code,
                    "duration_ms": out.duration_ms,
                    "tree": parsed,
                    "error": if out.success() { Value::Null } else { json!(out.stderr) },
                }))
            }
            "python" => {
                let out = run_cmd(
                    "pipdeptree",
                    &["--json"],
                    &ctx.project_root,
                    ctx.timeout_secs,
                )
                .await?;
                let parsed: Value = serde_json::from_str(&out.stdout)
                    .unwrap_or_else(|_| json!({"raw": out.stdout}));
                Ok(json!({
                    "ok": out.success(),
                    "stack": "python",
                    "exit_code": out.exit_code,
                    "duration_ms": out.duration_ms,
                    "tree": parsed,
                    "error": if out.success() { Value::Null } else { json!(out.stderr) },
                }))
            }
            _ => unreachable!(),
        }
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "depth": {"type": "integer", "minimum": 0, "description": "Max depth (rust/node)"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_stack_rust() {
        let tmp = std::env::temp_dir().join(format!("deps_tree_rust_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(detect_stack(&tmp), Some("rust"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_detect_stack_none() {
        let tmp = std::env::temp_dir().join(format!("deps_tree_none_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        assert_eq!(detect_stack(&tmp), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_safety_readonly() {
        assert!(DepsTreeTool.safety().read_only);
    }
}
