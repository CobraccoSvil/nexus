//! `dependencies::cargo_tree` — wrapper di `cargo tree`.
//!
//! Ritorna l'albero delle dipendenze del progetto Cargo in forma testuale
//! (come stampato da `cargo tree`) insieme a un conteggio aggregato per
//! feature type (direct, transitive, ecc. basato su indentazione).
//!
//! Input schema:
//! ```json
//! {
//!   "workspace_member": "string (optional)",
//!   "depth": "integer (optional, default none)",
//!   "edges": "string (optional, 'features'|'normal'|'build'|'dev'|'all')"
//! }
//! ```

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoTreeTool;

#[async_trait]
impl NexusToolHandler for CargoTreeTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let workspace_member = args
            .get("workspace_member")
            .and_then(Value::as_str)
            .map(|s| s.to_string());
        let depth = args.get("depth").and_then(Value::as_u64);
        let edges = args
            .get("edges")
            .and_then(Value::as_str)
            .map(|s| s.to_string());

        let mut cmd_args: Vec<String> = vec!["tree".to_string()];
        if let Some(member) = &workspace_member {
            cmd_args.push("-p".to_string());
            cmd_args.push(member.clone());
        }
        if let Some(d) = depth {
            cmd_args.push("--depth".to_string());
            cmd_args.push(d.to_string());
        }
        if let Some(e) = &edges {
            cmd_args.push("--edges".to_string());
            cmd_args.push(e.clone());
        }

        let refs: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();
        let out = run_cmd("cargo", &refs, &ctx.project_root, ctx.timeout_secs).await?;

        if !out.success() {
            return Err(NexusToolError::Exec {
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }

        let (total_lines, root_count) = count_tree_stats(&out.stdout);

        Ok(json!({
            "tree": out.stdout,
            "total_nodes": total_lines,
            "root_packages": root_count,
            "workspace_member": workspace_member,
            "duration_ms": out.duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "workspace_member": {"type": "string"},
                "depth": {"type": "integer", "minimum": 0},
                "edges": {"type": "string", "enum": ["features","normal","build","dev","all"]}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}

/// Conta le righe totali (= nodi dell'albero) e le "root" (righe senza
/// indentazione di ramo).
fn count_tree_stats(stdout: &str) -> (usize, usize) {
    let mut total = 0usize;
    let mut roots = 0usize;
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        total += 1;
        // Le righe "root" iniziano con una lettera/numero (nome package),
        // mentre le dipendenze iniziano con simboli di ramo tipo ├── │   └──
        let first = line.chars().next().unwrap_or(' ');
        if first.is_alphanumeric() {
            roots += 1;
        }
    }
    (total, roots)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_tree_stats() {
        let tree = "mcp-core v0.1.0 (/path)\n├── anyhow v1.0.75\n│   └── serde v1.0.190\n└── tokio v1.35.0\n";
        let (total, roots) = count_tree_stats(tree);
        assert_eq!(total, 4);
        assert_eq!(roots, 1); // solo "mcp-core" è una root
    }

    #[test]
    fn test_safety_readonly() {
        assert!(CargoTreeTool.safety().read_only);
    }
}
