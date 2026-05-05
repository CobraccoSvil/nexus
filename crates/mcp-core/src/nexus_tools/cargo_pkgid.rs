//! `build::cargo_pkgid` — `cargo pkgid` (resolved package URL).
//!
//! Output: `{pkgid, name?, version?}`.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoPkgidTool;

#[async_trait]
impl NexusToolHandler for CargoPkgidTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let mut cmd_args: Vec<&str> = vec!["pkgid"];
        let pkg_arg;
        if let Some(p) = args.get("package").and_then(Value::as_str) {
            pkg_arg = p.to_string();
            cmd_args.push("-p");
            cmd_args.push(&pkg_arg);
        }

        let out = run_cmd("cargo", &cmd_args, &ctx.project_root, ctx.timeout_secs).await?;
        if !out.success() {
            return Err(NexusToolError::Exec {
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }

        let pkgid = out.stdout.trim().to_string();
        // Format esempio: file:///path/to/proj#pkgname@1.2.3
        let (name, version) = if let Some(hash_idx) = pkgid.find('#') {
            let after = &pkgid[hash_idx + 1..];
            if let Some(at) = after.rfind('@') {
                (
                    Some(after[..at].to_string()),
                    Some(after[at + 1..].to_string()),
                )
            } else {
                (Some(after.to_string()), None)
            }
        } else {
            (None, None)
        };

        Ok(json!({
            "ok": true,
            "pkgid": pkgid,
            "name": name,
            "version": version,
            "duration_ms": out.duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "package": {"type": "string"}
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
    fn test_safety() {
        assert!(CargoPkgidTool.safety().read_only);
    }
}
