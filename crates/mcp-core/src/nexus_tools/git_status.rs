//! `vcs::git_status` — wrapper read-only di `git status --porcelain=v2 --branch`.
//!
//! Parsa il formato porcelain v2 (stabile, machine-readable) invece del
//! classico `--porcelain` per avere informazioni di branch/upstream/
//! ahead/behind in modo strutturato.
//!
//! Input schema: nessun argomento richiesto.
//!
//! Output:
//! ```json
//! {
//!   "branch": "main",
//!   "upstream": "origin/main",
//!   "ahead": 2,
//!   "behind": 0,
//!   "clean": false,
//!   "modified": ["src/a.rs"],
//!   "added":    ["src/b.rs"],
//!   "deleted":  [],
//!   "renamed":  [{"from": "x.rs", "to": "y.rs"}],
//!   "untracked": ["tmp.log"],
//!   "ignored": []
//! }
//! ```
//!
//! Riferimento porcelain v2:
//! - `# branch.head <name>`
//! - `# branch.upstream <name>`
//! - `# branch.ab +<ahead> -<behind>`
//! - `1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>` — tracked, non-rename
//! - `2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>\t<origPath>` — rename
//! - `u <XY> ...` — unmerged
//! - `? <path>` — untracked
//! - `! <path>` — ignored

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitStatusTool;

#[async_trait]
impl NexusToolHandler for GitStatusTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let out = run_cmd(
            "git",
            &["status", "--porcelain=v2", "--branch"],
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

        let parsed = parse_porcelain_v2(&out.stdout);

        Ok(json!({
            "branch": parsed.branch,
            "upstream": parsed.upstream,
            "ahead": parsed.ahead,
            "behind": parsed.behind,
            "clean": parsed.is_clean(),
            "modified": parsed.modified,
            "added": parsed.added,
            "deleted": parsed.deleted,
            "renamed": parsed.renamed,
            "untracked": parsed.untracked,
            "ignored": parsed.ignored,
            "duration_ms": out.duration_ms,
        }))
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}

/// Struttura intermedia popolata dal parser porcelain v2.
#[derive(Debug, Default)]
struct GitStatus {
    branch: Option<String>,
    upstream: Option<String>,
    ahead: i64,
    behind: i64,
    modified: Vec<String>,
    added: Vec<String>,
    deleted: Vec<String>,
    renamed: Vec<Value>,
    untracked: Vec<String>,
    ignored: Vec<String>,
}

impl GitStatus {
    fn is_clean(&self) -> bool {
        self.modified.is_empty()
            && self.added.is_empty()
            && self.deleted.is_empty()
            && self.renamed.is_empty()
            && self.untracked.is_empty()
    }
}

/// Classifica uno status XY porcelain v2 in una delle liste.
///
/// Layout XY: primo char = stato staged (index), secondo = stato worktree.
/// - 'M' modificato
/// - 'A' aggiunto
/// - 'D' cancellato
/// - 'R' rinominato (solo con entry type "2")
/// - '.' nessun change
///
/// Semplificazione: se uno dei due char è non-dot, il file è "changed" e
/// ci basa il primo carattere non-dot per classificare.
fn classify_xy(xy: &str) -> &'static str {
    let chars: Vec<char> = xy.chars().collect();
    let staged = chars.first().copied().unwrap_or('.');
    let worktree = chars.get(1).copied().unwrap_or('.');
    let primary = if staged != '.' { staged } else { worktree };
    match primary {
        'M' => "modified",
        'A' => "added",
        'D' => "deleted",
        'R' => "renamed",
        'C' => "added", // copy = aggiunto
        _ => "modified",
    }
}

fn parse_porcelain_v2(text: &str) -> GitStatus {
    let mut status = GitStatus::default();

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            status.branch = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("# branch.upstream ") {
            status.upstream = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            // Formato: "+N -M"
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() == 2 {
                status.ahead = parts[0].trim_start_matches('+').parse().unwrap_or(0);
                status.behind = parts[1].trim_start_matches('-').parse().unwrap_or(0);
            }
        } else if let Some(rest) = line.strip_prefix("1 ") {
            // Tracked non-rename: "<XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>"
            let fields: Vec<&str> = rest.splitn(8, ' ').collect();
            if fields.len() == 8 {
                let xy = fields[0];
                let path = fields[7].to_string();
                match classify_xy(xy) {
                    "modified" => status.modified.push(path),
                    "added" => status.added.push(path),
                    "deleted" => status.deleted.push(path),
                    _ => status.modified.push(path),
                }
            }
        } else if let Some(rest) = line.strip_prefix("2 ") {
            // Rename/copy: "<XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>\t<origPath>"
            let fields: Vec<&str> = rest.splitn(9, ' ').collect();
            if fields.len() == 9 {
                let path_and_orig = fields[8];
                // path<TAB>origPath
                let mut split = path_and_orig.splitn(2, '\t');
                let new_path = split.next().unwrap_or("").to_string();
                let old_path = split.next().unwrap_or("").to_string();
                status.renamed.push(json!({
                    "from": old_path,
                    "to": new_path,
                }));
            }
        } else if let Some(rest) = line.strip_prefix("? ") {
            status.untracked.push(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("! ") {
            status.ignored.push(rest.to_string());
        }
        // Ignoriamo unmerged "u ..." per Fase 9 — raro in CI interactive.
    }

    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_clean_main_branch() {
        let input = "# branch.oid abcdef\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +0 -0\n";
        let s = parse_porcelain_v2(input);
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.upstream.as_deref(), Some("origin/main"));
        assert_eq!(s.ahead, 0);
        assert_eq!(s.behind, 0);
        assert!(s.is_clean());
    }

    #[test]
    fn test_parse_modified_and_untracked() {
        let input = "# branch.head feat/x\n# branch.ab +2 -1\n1 .M N... 100644 100644 100644 abc def src/lib.rs\n? tmp.log\n";
        let s = parse_porcelain_v2(input);
        assert_eq!(s.branch.as_deref(), Some("feat/x"));
        assert_eq!(s.ahead, 2);
        assert_eq!(s.behind, 1);
        assert_eq!(s.modified, vec!["src/lib.rs".to_string()]);
        assert_eq!(s.untracked, vec!["tmp.log".to_string()]);
        assert!(!s.is_clean());
    }

    #[test]
    fn test_parse_rename() {
        let input = "# branch.head main\n2 R. N... 100644 100644 100644 abc def R100 new.rs\told.rs\n";
        let s = parse_porcelain_v2(input);
        assert_eq!(s.renamed.len(), 1);
        assert_eq!(s.renamed[0]["from"], "old.rs");
        assert_eq!(s.renamed[0]["to"], "new.rs");
    }

    #[test]
    fn test_parse_added_file() {
        let input = "# branch.head main\n1 A. N... 100644 100644 100644 0000 abc src/new.rs\n";
        let s = parse_porcelain_v2(input);
        assert_eq!(s.added, vec!["src/new.rs".to_string()]);
    }

    #[test]
    fn test_classify_xy() {
        assert_eq!(classify_xy(".M"), "modified");
        assert_eq!(classify_xy("A."), "added");
        assert_eq!(classify_xy("D."), "deleted");
        assert_eq!(classify_xy("MM"), "modified");
    }

    #[test]
    fn test_safety_is_readonly() {
        let s = GitStatusTool.safety();
        assert!(s.read_only);
        assert!(s.can_execute_subproc);
        assert!(!s.can_write_filesystem);
    }
}
