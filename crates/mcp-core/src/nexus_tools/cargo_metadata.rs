//! `code_analysis::cargo_metadata` — wrapper di `cargo metadata --format-version=1`.
//!
//! Ritorna il grafo completo dei package del workspace in forma strutturata
//! (parse diretto del JSON output di cargo). Estrae i nomi dei workspace
//! members e il conteggio delle dipendenze risolte. È la base per molti
//! altri handler (license_check, deps_analysis, ...).

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoMetadataTool;

#[async_trait]
impl NexusToolHandler for CargoMetadataTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let no_deps = args
            .get("no_deps")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let mut cmd_args: Vec<&str> = vec!["metadata", "--format-version=1"];
        if no_deps {
            cmd_args.push("--no-deps");
        }

        let out = run_cmd("cargo", &cmd_args, &ctx.project_root, ctx.timeout_secs).await?;

        if !out.success() {
            return Err(NexusToolError::Exec {
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }

        // Parse cargo output (single JSON blob)
        let metadata: Value = serde_json::from_str(&out.stdout)?;

        Ok(summarize_metadata(&metadata, out.duration_ms))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "no_deps": {
                    "type": "boolean",
                    "description": "Se true, esclude le dipendenze transitive dal grafo. Default: false."
                }
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}

/// Produce un sommario del cargo metadata senza duplicare la dimensione del
/// JSON originale (che può essere molto grande per workspace complessi).
/// Mantiene solo workspace_members, package names/versions, e total counts.
fn summarize_metadata(metadata: &Value, duration_ms: u64) -> Value {
    let workspace_members = metadata
        .get("workspace_members")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let package_summaries: Vec<Value> = packages
        .iter()
        .map(|p| {
            json!({
                "name": p.get("name").cloned().unwrap_or(Value::Null),
                "version": p.get("version").cloned().unwrap_or(Value::Null),
                "id": p.get("id").cloned().unwrap_or(Value::Null),
                "manifest_path": p.get("manifest_path").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();

    let workspace_root = metadata
        .get("workspace_root")
        .cloned()
        .unwrap_or(Value::Null);
    let target_directory = metadata
        .get("target_directory")
        .cloned()
        .unwrap_or(Value::Null);

    json!({
        "workspace_root": workspace_root,
        "target_directory": target_directory,
        "workspace_members": workspace_members,
        "packages": package_summaries,
        "total_packages": packages.len(),
        "total_workspace_members": workspace_members.len(),
        "duration_ms": duration_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_summarize_metadata_minimal() {
        let meta = json!({
            "workspace_root": "/tmp/proj",
            "target_directory": "/tmp/proj/target",
            "workspace_members": ["pkg-a 0.1.0 (path+file:///tmp/proj#pkg-a)"],
            "packages": [
                {"name": "pkg-a", "version": "0.1.0", "id": "id1", "manifest_path": "/tmp/proj/Cargo.toml"}
            ]
        });
        let sum = summarize_metadata(&meta, 10);
        assert_eq!(sum["total_packages"], 1);
        assert_eq!(sum["total_workspace_members"], 1);
        assert_eq!(sum["packages"][0]["name"], "pkg-a");
    }
}
