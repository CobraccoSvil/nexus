//! `vcs::git_show` — wrapper di `git show <ref>` con stats file-level.
//!
//! Input: `{ref?}` (default HEAD), `{stats_only?}` (default false)
//! Output: commit meta + file changes.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitShowTool;

fn is_valid_ref(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '/' | '.' | '~' | '^' | '@'))
}

#[async_trait]
impl NexusToolHandler for GitShowTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let rev = args.get("ref").and_then(Value::as_str).unwrap_or("HEAD");
        if !is_valid_ref(rev) {
            return Err(NexusToolError::BadInput(format!(
                "invalid ref '{}' — only [a-zA-Z0-9._/~^@-] allowed",
                rev
            )));
        }
        let stats_only = args.get("stats_only").and_then(Value::as_bool).unwrap_or(false);

        let fmt = "--format=%H%n%an <%ae>%n%at%n%s%n%b%n---END---";
        let mut cmd_args: Vec<&str> = vec!["show", fmt];
        if stats_only {
            cmd_args.push("--stat");
        } else {
            cmd_args.push("--numstat");
        }
        cmd_args.push(rev);

        let out = run_cmd("git", &cmd_args, &ctx.project_root, ctx.timeout_secs).await?;
        if !out.success() {
            return Err(NexusToolError::Exec {
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }

        // Parse fino a ---END---
        let mut lines = out.stdout.lines();
        let commit = lines.next().unwrap_or("").to_string();
        let author = lines.next().unwrap_or("").to_string();
        let timestamp = lines.next().and_then(|s| s.parse::<i64>().ok());
        let subject = lines.next().unwrap_or("").to_string();
        let mut body_lines: Vec<&str> = Vec::new();
        for l in lines.by_ref() {
            if l == "---END---" {
                break;
            }
            body_lines.push(l);
        }
        let body = body_lines.join("\n");

        // Rest = numstat
        let mut files: Vec<Value> = Vec::new();
        for l in lines {
            let parts: Vec<&str> = l.split_whitespace().collect();
            if parts.len() >= 3 {
                let added: Option<i64> = parts[0].parse().ok();
                let deleted: Option<i64> = parts[1].parse().ok();
                let path = parts[2..].join(" ");
                files.push(json!({
                    "path": path,
                    "added": added,
                    "deleted": deleted,
                }));
            }
        }

        Ok(json!({
            "ok": true,
            "ref": rev,
            "commit": commit,
            "author": author,
            "timestamp": timestamp,
            "subject": subject,
            "body": body,
            "files_changed": files.len(),
            "files": files,
            "duration_ms": out.duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "ref": {"type": "string"},
                "stats_only": {"type": "boolean"}
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
    fn test_valid_ref() {
        assert!(is_valid_ref("HEAD"));
        assert!(is_valid_ref("main"));
        assert!(is_valid_ref("origin/main"));
        assert!(is_valid_ref("abc123"));
        assert!(!is_valid_ref("foo;rm -rf /"));
        assert!(!is_valid_ref(""));
    }

    #[test]
    fn test_safety() {
        assert!(GitShowTool.safety().read_only);
    }
}
