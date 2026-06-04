//! `vcs::git_log` — wrapper di `git log` con formato strutturato.
//!
//! Usa `--format=<custom>` con delimitatori `\x1f` (unit separator) tra
//! campi e `\x1e` (record separator) tra commit per avere parsing robusto
//! anche con messaggi multi-riga.
//!
//! Input schema:
//! ```json
//! { "limit": 20, "path": "src/lib.rs" (opzionale), "author": "..." (opzionale) }
//! ```

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitLogTool;

const FIELD_SEP: char = '\x1f';
const RECORD_SEP: char = '\x1e';

#[async_trait]
impl NexusToolHandler for GitLogTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20);
        let path = args.get("path").and_then(Value::as_str).map(String::from);
        let author = args.get("author").and_then(Value::as_str).map(String::from);

        // Formato: hash\x1fauthor\x1femail\x1fdate_iso\x1fsubject\x1fbody\x1e
        let format = format!(
            "--format=%H{fs}%an{fs}%ae{fs}%aI{fs}%s{fs}%b{rs}",
            fs = FIELD_SEP,
            rs = RECORD_SEP
        );
        let limit_str = format!("-n{}", limit);

        let mut cmd: Vec<String> = vec!["log".into(), limit_str, format];
        if let Some(a) = &author {
            cmd.push(format!("--author={}", a));
        }
        if let Some(p) = &path {
            cmd.push("--".into());
            cmd.push(p.clone());
        }
        let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        let out = run_cmd("git", &refs, &ctx.project_root, ctx.timeout_secs).await?;

        if !out.success() {
            return Err(NexusToolError::Exec {
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }

        let commits = parse_git_log(&out.stdout);

        Ok(json!({
            "total": commits.len(),
            "commits": commits,
            "limit": limit,
            "path": path,
            "author": author,
            "duration_ms": out.duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "minimum": 1, "default": 20},
                "path": {"type": "string"},
                "author": {"type": "string"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}

fn parse_git_log(stdout: &str) -> Vec<Value> {
    stdout
        .split(RECORD_SEP)
        .map(|rec| rec.trim_matches('\n'))
        .filter(|rec| !rec.is_empty())
        .filter_map(|rec| {
            let fields: Vec<&str> = rec.split(FIELD_SEP).collect();
            if fields.len() < 5 {
                return None;
            }
            Some(json!({
                "hash": fields[0],
                "author": fields[1],
                "email": fields[2],
                "date": fields[3],
                "subject": fields[4],
                "body": fields.get(5).copied().unwrap_or(""),
            }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_two_commits() {
        let stdout = format!(
            "abc123{fs}Alice{fs}alice@x{fs}2026-04-14T10:00:00Z{fs}fix bug{fs}body1{rs}def456{fs}Bob{fs}bob@x{fs}2026-04-13T09:00:00Z{fs}feat{fs}body2{rs}",
            fs = FIELD_SEP,
            rs = RECORD_SEP
        );
        let commits = parse_git_log(&stdout);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0]["hash"], "abc123");
        assert_eq!(commits[0]["author"], "Alice");
        assert_eq!(commits[1]["subject"], "feat");
    }
}
