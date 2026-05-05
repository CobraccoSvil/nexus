//! `code_quality::lint_run` — dispatcher generico per linter multi-linguaggio.
//!
//! Strategia:
//! - `Cargo.toml` → `cargo clippy --message-format=json`
//! - `package.json` con script `lint` → `npm run lint`
//! - `package.json` senza script ma con eslint config → `npx eslint .`
//! - `pyproject.toml`|`requirements.txt` → `ruff check` (fallback `flake8`)
//!
//! Ritorna `{ok, stack, linter, diagnostics_count, stdout, stderr}`.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct LintRunTool;

fn count_clippy_messages(stdout: &str) -> usize {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("reason").and_then(Value::as_str) == Some("compiler-message"))
        .count()
}

#[async_trait]
impl NexusToolHandler for LintRunTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let root = &ctx.project_root;

        if root.join("Cargo.toml").is_file() {
            let out = run_cmd(
                "cargo",
                &["clippy", "--message-format=json", "--quiet"],
                root,
                ctx.timeout_secs,
            )
            .await?;
            return Ok(json!({
                "ok": out.success(),
                "stack": "rust",
                "linter": "clippy",
                "exit_code": out.exit_code,
                "duration_ms": out.duration_ms,
                "diagnostics_count": count_clippy_messages(&out.stdout),
                "stderr": out.stderr,
            }));
        }

        if root.join("package.json").is_file() {
            let pj: Value = serde_json::from_str(
                &std::fs::read_to_string(root.join("package.json")).unwrap_or_default(),
            )
            .unwrap_or(Value::Null);
            let has_lint = pj
                .get("scripts")
                .and_then(|s| s.get("lint"))
                .and_then(Value::as_str)
                .is_some();
            if has_lint {
                let out = run_cmd("npm", &["run", "lint"], root, ctx.timeout_secs).await?;
                return Ok(json!({
                    "ok": out.success(),
                    "stack": "node",
                    "linter": "npm-run-lint",
                    "exit_code": out.exit_code,
                    "duration_ms": out.duration_ms,
                    "stdout": out.stdout,
                    "stderr": out.stderr,
                }));
            }
            // Fallback: eslint direct
            let has_eslint_cfg = [
                ".eslintrc",
                ".eslintrc.js",
                ".eslintrc.cjs",
                ".eslintrc.json",
                ".eslintrc.yml",
                "eslint.config.js",
                "eslint.config.mjs",
            ]
            .iter()
            .any(|f| root.join(f).is_file());
            if has_eslint_cfg {
                let out = run_cmd("npx", &["eslint", "."], root, ctx.timeout_secs).await?;
                return Ok(json!({
                    "ok": out.success(),
                    "stack": "node",
                    "linter": "eslint",
                    "exit_code": out.exit_code,
                    "duration_ms": out.duration_ms,
                    "stdout": out.stdout,
                    "stderr": out.stderr,
                }));
            }
            return Ok(json!({
                "ok": false,
                "stack": "node",
                "error": "No 'lint' npm script and no eslint config found",
            }));
        }

        if root.join("pyproject.toml").is_file() || root.join("requirements.txt").is_file() {
            // Prova ruff, poi flake8 come fallback
            let ruff = run_cmd("ruff", &["check", "."], root, ctx.timeout_secs).await;
            if let Ok(out) = ruff {
                return Ok(json!({
                    "ok": out.success(),
                    "stack": "python",
                    "linter": "ruff",
                    "exit_code": out.exit_code,
                    "duration_ms": out.duration_ms,
                    "stdout": out.stdout,
                    "stderr": out.stderr,
                }));
            }
            let flake = run_cmd("flake8", &["."], root, ctx.timeout_secs).await?;
            return Ok(json!({
                "ok": flake.success(),
                "stack": "python",
                "linter": "flake8",
                "exit_code": flake.exit_code,
                "duration_ms": flake.duration_ms,
                "stdout": flake.stdout,
                "stderr": flake.stderr,
            }));
        }

        Ok(json!({
            "ok": false,
            "stack": "unknown",
            "error": "No supported manifest found for linting",
        }))
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_clippy_messages() {
        let stdout = r#"{"reason":"compiler-message","message":{}}
{"reason":"compiler-artifact"}
{"reason":"compiler-message","message":{}}
"#;
        assert_eq!(count_clippy_messages(stdout), 2);
    }

    #[test]
    fn test_safety_readonly() {
        assert!(LintRunTool.safety().read_only);
    }
}
