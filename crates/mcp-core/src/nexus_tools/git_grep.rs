//! `vcs::git_grep` — `git grep -n -E <pattern>` (search nei file tracked).
//!
//! Input: `{pattern, max_matches?, case_insensitive?}`.
//! Output: `{count, items: [{path, line, text}]}`.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitGrepTool;

#[async_trait]
impl NexusToolHandler for GitGrepTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let pattern = args
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("pattern required".into()))?;
        let case_insensitive = args
            .get("case_insensitive")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let max_matches = args
            .get("max_matches")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(500)
            .min(5000);

        let mut cmd_args: Vec<&str> = vec!["grep", "-n", "-E", "--no-color"];
        if case_insensitive {
            cmd_args.push("-i");
        }
        cmd_args.push("--");
        cmd_args.push(pattern);

        let out = run_cmd("git", &cmd_args, &ctx.project_root, ctx.timeout_secs).await?;

        // git grep exit 1 = nessun match (legittimo)
        if !out.success() && out.exit_code != 1 {
            return Err(NexusToolError::Exec {
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }

        let mut items = Vec::new();
        for line in out.stdout.lines() {
            if items.len() >= max_matches {
                break;
            }
            // format: path:line:text
            let mut parts = line.splitn(3, ':');
            let path = parts.next().unwrap_or("");
            let line_no = parts.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            let text = parts.next().unwrap_or("");
            if !path.is_empty() {
                items.push(json!({
                    "path": path,
                    "line": line_no,
                    "text": text.chars().take(300).collect::<String>(),
                }));
            }
        }

        Ok(json!({
            "ok": true,
            "pattern": pattern,
            "count": items.len(),
            "truncated": items.len() >= max_matches,
            "items": items,
            "duration_ms": out.duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": {"type": "string"},
                "case_insensitive": {"type": "boolean"},
                "max_matches": {"type": "integer", "minimum": 1, "maximum": 5000}
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
        assert!(GitGrepTool.safety().read_only);
    }
}
