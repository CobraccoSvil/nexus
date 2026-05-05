//! `github::gh_pr_create` — wrapper di `gh pr create`.
//!
//! Crea una pull request sul repository corrente usando il CLI `gh`.
//! Richiede `gh` installato e autenticato.
//!
//! Input:
//! - `title` (required)
//! - `body` (optional, default "")
//! - `base` (optional, branch target — default "main")
//! - `head` (optional, branch source — default current)
//! - `draft` (optional, bool)
//! - `repo` (optional, "owner/name")
//!
//! Output: `{ok, url, number, error}`.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhPrCreateTool;

fn extract_pr_url(stdout: &str) -> Option<&str> {
    stdout
        .lines()
        .find(|l| l.contains("github.com") && l.contains("/pull/"))
        .map(|l| l.trim())
}

fn extract_pr_number(url: &str) -> Option<u64> {
    url.rsplit('/')
        .next()
        .and_then(|s| s.parse::<u64>().ok())
}

#[async_trait]
impl NexusToolHandler for GhPrCreateTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let title = args
            .get("title")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("title required".into()))?
            .to_string();
        let body = args
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let base = args.get("base").and_then(Value::as_str).map(String::from);
        let head = args.get("head").and_then(Value::as_str).map(String::from);
        let draft = args.get("draft").and_then(Value::as_bool).unwrap_or(false);
        let repo = args.get("repo").and_then(Value::as_str).map(String::from);

        let mut cmd: Vec<String> = vec![
            "pr".into(),
            "create".into(),
            "--title".into(),
            title,
            "--body".into(),
            body,
        ];
        if let Some(b) = base {
            cmd.push("--base".into());
            cmd.push(b);
        }
        if let Some(h) = head {
            cmd.push("--head".into());
            cmd.push(h);
        }
        if draft {
            cmd.push("--draft".into());
        }
        if let Some(r) = repo {
            cmd.push("--repo".into());
            cmd.push(r);
        }
        let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        let out = run_cmd("gh", &refs, &ctx.project_root, ctx.timeout_secs).await?;

        if !out.success() {
            return Ok(json!({
                "ok": false,
                "exit_code": out.exit_code,
                "duration_ms": out.duration_ms,
                "error": out.stderr.trim().to_string(),
                "hint": "Verifica che `gh auth status` sia OK e che il branch corrente non sia main.",
            }));
        }

        let url = extract_pr_url(&out.stdout).unwrap_or("").to_string();
        let number = extract_pr_number(&url);

        Ok(json!({
            "ok": true,
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "url": url,
            "number": number,
            "raw": out.stdout.trim().to_string(),
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["title"],
            "properties": {
                "title": {"type": "string"},
                "body": {"type": "string"},
                "base": {"type": "string"},
                "head": {"type": "string"},
                "draft": {"type": "boolean"},
                "repo": {"type": "string", "description": "owner/name"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: false,
            can_write_filesystem: false,
            can_execute_subproc: true,
            network_egress: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_pr_url() {
        let out = "Creating pull request for feature-x\nhttps://github.com/org/repo/pull/42\n";
        assert_eq!(
            extract_pr_url(out),
            Some("https://github.com/org/repo/pull/42")
        );
    }

    #[test]
    fn test_extract_pr_number() {
        assert_eq!(
            extract_pr_number("https://github.com/org/repo/pull/42"),
            Some(42)
        );
    }

    #[test]
    fn test_safety_writes_remote() {
        let s = GhPrCreateTool.safety();
        assert!(!s.read_only);
        assert!(s.network_egress);
    }
}
