//! `utility::text_diff` — diff line-based semplice tra due testi.
//!
//! Implementa LCS naive O(n*m) — sufficiente per file < ~2k linee.
//! Output: `{added, removed, hunks: [{tag, line, text}]}`.
//!
//! Input: `{a, b}` due stringhe, oppure `{path_a, path_b}` due file.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct TextDiffTool;

fn lcs_diff(a: &[&str], b: &[&str]) -> Vec<(char, usize, String)> {
    // tag: '=' equal, '+' added (in b), '-' removed (in a)
    let n = a.len();
    let m = b.len();
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in 0..n {
        for j in 0..m {
            dp[i + 1][j + 1] = if a[i] == b[j] {
                dp[i][j] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let mut i = n;
    let mut j = m;
    while i > 0 && j > 0 {
        if a[i - 1] == b[j - 1] {
            out.push(('=', j, a[i - 1].to_string()));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            out.push(('-', i, a[i - 1].to_string()));
            i -= 1;
        } else {
            out.push(('+', j, b[j - 1].to_string()));
            j -= 1;
        }
    }
    while i > 0 {
        out.push(('-', i, a[i - 1].to_string()));
        i -= 1;
    }
    while j > 0 {
        out.push(('+', j, b[j - 1].to_string()));
        j -= 1;
    }
    out.reverse();
    out
}

#[async_trait]
impl NexusToolHandler for TextDiffTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let a_text: String = if let Some(s) = args.get("a").and_then(Value::as_str) {
            s.to_string()
        } else if let Some(p) = args.get("path_a").and_then(Value::as_str) {
            let full = ctx.project_root.join(p);
            if !full.starts_with(&ctx.project_root) {
                return Err(NexusToolError::BadInput("path traversal denied".into()));
            }
            std::fs::read_to_string(&full).map_err(NexusToolError::Io)?
        } else {
            return Err(NexusToolError::BadInput("a or path_a required".into()));
        };
        let b_text: String = if let Some(s) = args.get("b").and_then(Value::as_str) {
            s.to_string()
        } else if let Some(p) = args.get("path_b").and_then(Value::as_str) {
            let full = ctx.project_root.join(p);
            if !full.starts_with(&ctx.project_root) {
                return Err(NexusToolError::BadInput("path traversal denied".into()));
            }
            std::fs::read_to_string(&full).map_err(NexusToolError::Io)?
        } else {
            return Err(NexusToolError::BadInput("b or path_b required".into()));
        };

        let a_lines: Vec<&str> = a_text.lines().collect();
        let b_lines: Vec<&str> = b_text.lines().collect();

        if a_lines.len() > 5000 || b_lines.len() > 5000 {
            return Err(NexusToolError::BadInput(
                "input too large for naive diff (max 5000 lines)".into(),
            ));
        }

        let diff = lcs_diff(&a_lines, &b_lines);
        let mut added = 0;
        let mut removed = 0;
        let mut hunks = Vec::new();
        for (tag, line, text) in diff.iter() {
            if *tag == '+' {
                added += 1;
            } else if *tag == '-' {
                removed += 1;
            }
            // emette solo le diff non-equal per compattezza
            if *tag != '=' {
                hunks.push(json!({
                    "tag": tag.to_string(),
                    "line": line,
                    "text": text.chars().take(300).collect::<String>(),
                }));
            }
        }
        let unchanged = diff.len() - added - removed;

        Ok(json!({
            "ok": true,
            "added": added,
            "removed": removed,
            "unchanged": unchanged,
            "a_lines": a_lines.len(),
            "b_lines": b_lines.len(),
            "hunks": hunks,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "a": {"type": "string"},
                "b": {"type": "string"},
                "path_a": {"type": "string"},
                "path_b": {"type": "string"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_text_diff_basic() {
        let ctx = NexusToolContext::new(std::env::temp_dir(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = TextDiffTool
            .execute(&ctx, &json!({"a": "x\ny\nz", "b": "x\nY\nz"}))
            .await
            .unwrap();
        assert_eq!(out["added"], 1);
        assert_eq!(out["removed"], 1);
        assert_eq!(out["unchanged"], 2);
    }
}
