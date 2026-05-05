//! `vcs::git_shortlog` — `git shortlog -sne` (commit count per autore).
//!
//! Output: `{total_commits, total_authors, top: [{author, email, commits}]}`.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitShortlogTool;

#[async_trait]
impl NexusToolHandler for GitShortlogTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(50)
            .min(500);

        let out = run_cmd(
            "git",
            &["shortlog", "-sne", "--all", "HEAD"],
            &ctx.project_root,
            ctx.timeout_secs,
        )
        .await?;

        if !out.success() {
            return Err(NexusToolError::Exec {
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }

        let mut entries: Vec<(u32, String, String)> = Vec::new();
        let mut total_commits: u64 = 0;
        for line in out.stdout.lines() {
            let trimmed = line.trim_start();
            // format: "    42  Name <email@example.com>"
            let mut split = trimmed.splitn(2, char::is_whitespace);
            let count = split.next().and_then(|s| s.parse::<u32>().ok());
            let rest = split.next().unwrap_or("").trim();
            if let Some(c) = count {
                total_commits += c as u64;
                // parse "Name <email>"
                let (name, email) = if let Some(start) = rest.rfind('<') {
                    let name = rest[..start].trim().to_string();
                    let email = rest[start + 1..]
                        .trim_end_matches('>')
                        .to_string();
                    (name, email)
                } else {
                    (rest.to_string(), String::new())
                };
                entries.push((c, name, email));
            }
        }
        entries.sort_by(|a, b| b.0.cmp(&a.0));
        let total_authors = entries.len();
        let top: Vec<Value> = entries
            .into_iter()
            .take(limit)
            .map(|(c, n, e)| json!({"author": n, "email": e, "commits": c}))
            .collect();

        Ok(json!({
            "ok": true,
            "total_commits": total_commits,
            "total_authors": total_authors,
            "top": top,
            "duration_ms": out.duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "minimum": 1, "maximum": 500}
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
        assert!(GitShortlogTool.safety().read_only);
    }
}
