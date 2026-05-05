//! `vcs::git_diff` — wrapper di `git diff [--stat] [path]`.
//!
//! Ritorna il diff testuale completo e, in parallelo, la versione `--stat`
//! per un sommario numerico (files changed, insertions, deletions). Se
//! `staged` è true usa `git diff --cached` (index vs HEAD).

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitDiffTool;

#[async_trait]
impl NexusToolHandler for GitDiffTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let path = args.get("path").and_then(Value::as_str).map(String::from);
        let staged = args.get("staged").and_then(Value::as_bool).unwrap_or(false);
        let revision = args
            .get("revision")
            .and_then(Value::as_str)
            .map(String::from);

        // 1. Stat-only per il summary
        let mut stat_cmd: Vec<String> = vec!["diff".into(), "--stat".into()];
        if staged {
            stat_cmd.push("--cached".into());
        }
        if let Some(r) = &revision {
            stat_cmd.push(r.clone());
        }
        if let Some(p) = &path {
            stat_cmd.push("--".into());
            stat_cmd.push(p.clone());
        }
        let stat_refs: Vec<&str> = stat_cmd.iter().map(|s| s.as_str()).collect();
        let stat_out = run_cmd("git", &stat_refs, &ctx.project_root, ctx.timeout_secs).await?;

        if !stat_out.success() {
            return Err(NexusToolError::Exec {
                exit_code: stat_out.exit_code,
                stderr: stat_out.stderr,
            });
        }

        let stat = parse_diff_stat(&stat_out.stdout);

        // 2. Diff testuale completo
        let mut diff_cmd: Vec<String> = vec!["diff".into()];
        if staged {
            diff_cmd.push("--cached".into());
        }
        if let Some(r) = &revision {
            diff_cmd.push(r.clone());
        }
        if let Some(p) = &path {
            diff_cmd.push("--".into());
            diff_cmd.push(p.clone());
        }
        let diff_refs: Vec<&str> = diff_cmd.iter().map(|s| s.as_str()).collect();
        let diff_out = run_cmd("git", &diff_refs, &ctx.project_root, ctx.timeout_secs).await?;

        Ok(json!({
            "files_changed": stat.files_changed,
            "insertions": stat.insertions,
            "deletions": stat.deletions,
            "diff": diff_out.stdout,
            "staged": staged,
            "path": path,
            "revision": revision,
            "duration_ms": stat_out.duration_ms + diff_out.duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "staged": {"type": "boolean", "description": "Se true, usa git diff --cached"},
                "revision": {"type": "string", "description": "Revisione target (es. 'HEAD~1', 'main')"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}

#[derive(Default)]
struct DiffStat {
    files_changed: usize,
    insertions: usize,
    deletions: usize,
}

/// Parsa l'ultima riga di `git diff --stat`, formato tipico:
/// `5 files changed, 123 insertions(+), 45 deletions(-)`
fn parse_diff_stat(stdout: &str) -> DiffStat {
    let mut s = DiffStat::default();
    let last = stdout.lines().filter(|l| !l.trim().is_empty()).last();
    let Some(last) = last else {
        return s;
    };
    for segment in last.split(',') {
        let segment = segment.trim();
        if let Some(n_str) = segment.split_whitespace().next() {
            let n: usize = n_str.parse().unwrap_or(0);
            if segment.contains("file") {
                s.files_changed = n;
            } else if segment.contains("insertion") {
                s.insertions = n;
            } else if segment.contains("deletion") {
                s.deletions = n;
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_diff_stat() {
        let out = " src/lib.rs | 10 ++++++++--\n src/main.rs | 5 +++--\n 2 files changed, 12 insertions(+), 3 deletions(-)\n";
        let s = parse_diff_stat(out);
        assert_eq!(s.files_changed, 2);
        assert_eq!(s.insertions, 12);
        assert_eq!(s.deletions, 3);
    }

    #[test]
    fn test_parse_diff_stat_empty() {
        let s = parse_diff_stat("");
        assert_eq!(s.files_changed, 0);
        assert_eq!(s.insertions, 0);
    }
}
