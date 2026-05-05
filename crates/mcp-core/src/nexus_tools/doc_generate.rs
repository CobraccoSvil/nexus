//! `documentation::doc_generate` — generazione documentazione.
//!
//! Per progetti Rust: `cargo doc --no-deps`.
//! Per altri stack: ritorna un errore strutturato con hint (piuttosto che
//! tentare di ingegnerizzare un generator LLM senza infrastruttura presente).
//!
//! Output: `{ok, target_dir, duration_ms}` o errore strutturato.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DocGenerateTool;

#[async_trait]
impl NexusToolHandler for DocGenerateTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let root = &ctx.project_root;
        let open = args.get("open").and_then(Value::as_bool).unwrap_or(false);
        let include_deps = args
            .get("include_deps")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if root.join("Cargo.toml").is_file() {
            let mut cmd: Vec<String> = vec!["doc".into()];
            if !include_deps {
                cmd.push("--no-deps".into());
            }
            if open {
                cmd.push("--open".into());
            }
            let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
            let out = run_cmd("cargo", &refs, root, ctx.timeout_secs).await?;
            return Ok(json!({
                "ok": out.success(),
                "stack": "rust",
                "engine": "cargo-doc",
                "exit_code": out.exit_code,
                "duration_ms": out.duration_ms,
                "target_dir": "target/doc",
                "stderr": out.stderr,
            }));
        }

        if root.join("package.json").is_file() {
            // Prova tsdoc se typedoc presente, altrimenti suggest
            let pj: Value = serde_json::from_str(
                &std::fs::read_to_string(root.join("package.json")).unwrap_or_default(),
            )
            .unwrap_or(Value::Null);
            let has_doc_script = pj
                .get("scripts")
                .and_then(|s| s.get("docs"))
                .and_then(Value::as_str)
                .is_some();
            if has_doc_script {
                let out = run_cmd("npm", &["run", "docs"], root, ctx.timeout_secs).await?;
                return Ok(json!({
                    "ok": out.success(),
                    "stack": "node",
                    "engine": "npm-run-docs",
                    "exit_code": out.exit_code,
                    "duration_ms": out.duration_ms,
                    "stdout": out.stdout,
                    "stderr": out.stderr,
                }));
            }
            return Ok(json!({
                "ok": false,
                "stack": "node",
                "error": "No 'docs' npm script found",
                "hint": "Add a 'docs' script (es. typedoc) in package.json",
            }));
        }

        if root.join("pyproject.toml").is_file() {
            // Python: prova sphinx-build se docs/ esiste
            if root.join("docs").join("conf.py").is_file() {
                let out = run_cmd(
                    "sphinx-build",
                    &["-b", "html", "docs", "docs/_build/html"],
                    root,
                    ctx.timeout_secs,
                )
                .await?;
                return Ok(json!({
                    "ok": out.success(),
                    "stack": "python",
                    "engine": "sphinx",
                    "exit_code": out.exit_code,
                    "duration_ms": out.duration_ms,
                    "target_dir": "docs/_build/html",
                }));
            }
            return Ok(json!({
                "ok": false,
                "stack": "python",
                "error": "No docs/conf.py found for sphinx",
                "hint": "Run `sphinx-quickstart docs` to scaffold",
            }));
        }

        Ok(json!({
            "ok": false,
            "stack": "unknown",
            "error": "No supported project manifest found",
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "include_deps": {"type": "boolean", "description": "Include dependencies (cargo doc without --no-deps)"},
                "open": {"type": "boolean", "description": "Open the generated docs in a browser (cargo)"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::write_subproc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety_writes() {
        assert!(DocGenerateTool.safety().can_write_filesystem);
    }
}
