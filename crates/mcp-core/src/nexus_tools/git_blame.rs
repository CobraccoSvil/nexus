//! `vcs::git_blame` — wrapper di `git blame --porcelain <file>`.
//!
//! Usa il formato porcelain di git blame per avere parsing robusto:
//! ogni linea origina da una "section" che inizia con `<sha> <orig_line>
//! <final_line> [num_lines]` seguita da metadata (`author`, `summary`, ...).
//!
//! Output: array di `{line, sha, author, summary, content}` per ogni linea
//! del file target.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;

pub struct GitBlameTool;

#[async_trait]
impl NexusToolHandler for GitBlameTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("path is required".to_string()))?
            .to_string();
        let revision = args
            .get("revision")
            .and_then(Value::as_str)
            .map(String::from);

        let mut cmd: Vec<String> = vec!["blame".into(), "--porcelain".into()];
        if let Some(r) = &revision {
            cmd.push(r.clone());
        }
        cmd.push("--".into());
        cmd.push(path.clone());
        let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        let out = run_cmd("git", &refs, &ctx.project_root, ctx.timeout_secs).await?;

        if !out.success() {
            return Err(NexusToolError::Exec {
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }

        let lines = parse_blame_porcelain(&out.stdout);

        Ok(json!({
            "path": path,
            "revision": revision,
            "total_lines": lines.len(),
            "lines": lines,
            "duration_ms": out.duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {"type": "string"},
                "revision": {"type": "string"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}

fn parse_blame_porcelain(stdout: &str) -> Vec<Value> {
    let mut result = Vec::new();
    // Cache metadata per sha (porcelain ripete i metadata solo la prima volta)
    let mut meta_cache: HashMap<String, (String, String)> = HashMap::new();

    let mut iter = stdout.lines().peekable();
    while let Some(header) = iter.next() {
        // Header: "<sha> <orig_line> <final_line> [num_lines]"
        let parts: Vec<&str> = header.split_whitespace().collect();
        if parts.len() < 3 {
            continue;
        }
        let sha = parts[0].to_string();
        let final_line: u64 = parts[2].parse().unwrap_or(0);

        let mut author = String::new();
        let mut summary = String::new();

        // Consuma metadata fino a "\t<content>"
        while let Some(line) = iter.peek() {
            if line.starts_with('\t') {
                // Questa è la riga di contenuto, la prendiamo e usciamo
                break;
            }
            // peek() era Some, quindi next() lo e' a sua volta.
            let Some(line) = iter.next() else { break };
            if let Some(rest) = line.strip_prefix("author ") {
                author = rest.to_string();
            } else if let Some(rest) = line.strip_prefix("summary ") {
                summary = rest.to_string();
            }
        }

        // Se questo sha non aveva metadata espliciti, prendi dalla cache
        if author.is_empty() || summary.is_empty() {
            if let Some((a, s)) = meta_cache.get(&sha) {
                if author.is_empty() {
                    author = a.clone();
                }
                if summary.is_empty() {
                    summary = s.clone();
                }
            }
        } else {
            meta_cache.insert(sha.clone(), (author.clone(), summary.clone()));
        }

        // Contenuto (riga che inizia con \t)
        let content = iter
            .next()
            .and_then(|l| l.strip_prefix('\t'))
            .unwrap_or("")
            .to_string();

        result.push(json!({
            "line": final_line,
            "sha": sha,
            "author": author,
            "summary": summary,
            "content": content,
        }));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_blame_two_lines_same_commit() {
        let stdout = "abc123 1 1 2\nauthor Alice\nauthor-mail <a@x>\nsummary add foo\n\tlet x = 1;\nabc123 2 2\n\tlet y = 2;\n";
        let lines = parse_blame_porcelain(stdout);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["line"], 1);
        assert_eq!(lines[0]["author"], "Alice");
        assert_eq!(lines[0]["content"], "let x = 1;");
        assert_eq!(lines[1]["line"], 2);
        // Il secondo non ripete author/summary → deve ereditarli dalla cache
        assert_eq!(lines[1]["author"], "Alice");
        assert_eq!(lines[1]["summary"], "add foo");
    }
}
