//! `build::build_project` — dispatcher generico che esegue il build del
//! progetto rilevando lo stack.
//!
//! Strategia:
//! - `Cargo.toml` → `cargo build [--release]`
//! - `package.json` con script `build` → `npm run build`
//! - `Makefile` → `make`
//! - `pyproject.toml` → `python -m build`
//!
//! Ritorna exit_code/duration/stdout/stderr troncati + success flag.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct BuildProjectTool;

fn detect_build_kind(root: &std::path::Path) -> Option<&'static str> {
    if root.join("Cargo.toml").is_file() {
        Some("cargo")
    } else if root.join("package.json").is_file() {
        Some("npm")
    } else if root.join("Makefile").is_file() || root.join("makefile").is_file() {
        Some("make")
    } else if root.join("pyproject.toml").is_file() {
        Some("python")
    } else {
        None
    }
}

fn has_npm_build_script(package_json: &str) -> bool {
    let v: Value = match serde_json::from_str(package_json) {
        Ok(v) => v,
        Err(_) => return false,
    };
    v.get("scripts")
        .and_then(|s| s.get("build"))
        .and_then(Value::as_str)
        .is_some()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}\n...[truncated {} bytes]", &s[..max], s.len() - max)
    }
}

#[async_trait]
impl NexusToolHandler for BuildProjectTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let release = args.get("release").and_then(Value::as_bool).unwrap_or(false);
        let kind = match detect_build_kind(&ctx.project_root) {
            Some(k) => k,
            None => {
                return Ok(json!({
                    "ok": false,
                    "stack": "unknown",
                    "error": "No supported build manifest found (Cargo.toml, package.json, Makefile, pyproject.toml)",
                }));
            }
        };

        let (bin, cmd_args): (&'static str, Vec<String>) = match kind {
            "cargo" => {
                let mut a: Vec<String> = vec!["build".into()];
                if release {
                    a.push("--release".into());
                }
                ("cargo", a)
            }
            "npm" => {
                // Leggi package.json e controlla presenza script build
                let pj_path = ctx.project_root.join("package.json");
                let content = std::fs::read_to_string(&pj_path).unwrap_or_default();
                if !has_npm_build_script(&content) {
                    return Ok(json!({
                        "ok": false,
                        "stack": "node",
                        "error": "package.json has no 'build' script defined",
                    }));
                }
                ("npm", vec!["run".into(), "build".into()])
            }
            "make" => ("make", vec![]),
            "python" => ("python", vec!["-m".into(), "build".into()]),
            _ => unreachable!(),
        };

        let refs: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();
        let out = run_cmd(bin, &refs, &ctx.project_root, ctx.timeout_secs).await?;

        Ok(json!({
            "ok": out.success(),
            "stack": kind,
            "binary": bin,
            "release": release,
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "stdout": truncate(&out.stdout, 32_768),
            "stderr": truncate(&out.stderr, 32_768),
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "release": {"type": "boolean", "description": "Build in release mode (cargo)"}
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
    fn test_detect_build_kind() {
        let tmp = std::env::temp_dir().join(format!("bp_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("Cargo.toml"), "").unwrap();
        assert_eq!(detect_build_kind(&tmp), Some("cargo"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_has_npm_build_script_present() {
        let pj = r#"{"scripts": {"build": "tsc && vite build", "test": "jest"}}"#;
        assert!(has_npm_build_script(pj));
    }

    #[test]
    fn test_has_npm_build_script_missing() {
        let pj = r#"{"scripts": {"test": "jest"}}"#;
        assert!(!has_npm_build_script(pj));
    }

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("abc", 10), "abc");
    }

    #[test]
    fn test_truncate_long() {
        let s = "a".repeat(100);
        let t = truncate(&s, 20);
        assert!(t.contains("truncated"));
    }

    #[test]
    fn test_safety_writes() {
        assert!(BuildProjectTool.safety().can_write_filesystem);
    }
}
