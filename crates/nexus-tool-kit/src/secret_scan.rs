//! `security::secret_scan` — scanner regex-based di credenziali e secret
//! nei file di testo del progetto.
//!
//! A differenza degli altri handler questo NON lancia un subprocess: usa
//! direttamente la `regex` crate già in dipendenza e `walkdir` via
//! `std::fs::read_dir`. Scansiona i file di testo sotto la project root
//! (escludendo `target/`, `.git/`, `node_modules/`) e cerca pattern noti di:
//! - API key generiche (AWS_ACCESS_KEY, GitHub tokens)
//! - Private key PEM
//! - JWT tokens
//! - Password/secret hardcoded (heuristic)
//!
//! Output:
//! ```json
//! { "findings": [{"file": "...", "line": 42, "rule": "aws_access_key", "preview": "AKIA..."}] }
//! ```

// safety: tutte le `Regex::new("...").unwrap()` nei pattern di scansione
// secret sono literal hardcoded ammessi da CLAUDE.md §F. Refactor opportuno
// (LazyLock<Regex>) ma non e' una violazione.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};
use std::path::Path;

pub struct SecretScanTool;

struct Rule {
    name: &'static str,
    pattern: Regex,
}

fn build_rules() -> Vec<Rule> {
    vec![
        Rule {
            name: "aws_access_key",
            pattern: Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),
        },
        Rule {
            name: "aws_secret_key",
            pattern: Regex::new(
                r#"(?i)aws_secret_access_key[^\n]{0,20}['"][A-Za-z0-9/+=]{40}['"]"#,
            )
            .unwrap(),
        },
        Rule {
            name: "github_token",
            pattern: Regex::new(r"gh[pousr]_[A-Za-z0-9]{36,}").unwrap(),
        },
        Rule {
            name: "private_key_pem",
            pattern: Regex::new(r"-----BEGIN (RSA|EC|OPENSSH|PGP|DSA) PRIVATE KEY-----").unwrap(),
        },
        Rule {
            name: "jwt_token",
            pattern: Regex::new(
                r"eyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}",
            )
            .unwrap(),
        },
        Rule {
            name: "hardcoded_password",
            pattern: Regex::new(r#"(?i)(password|passwd|pwd)\s*[=:]\s*['"][^'"]{6,}['"]"#).unwrap(),
        },
        Rule {
            name: "slack_token",
            pattern: Regex::new(r"xox[baprs]-[A-Za-z0-9-]{10,}").unwrap(),
        },
    ]
}

#[async_trait]
impl NexusToolHandler for SecretScanTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let max_files = args
            .get("max_files")
            .and_then(Value::as_u64)
            .unwrap_or(2000) as usize;

        let rules = build_rules();
        let start = std::time::Instant::now();
        let mut findings: Vec<Value> = Vec::new();
        let mut files_scanned = 0usize;

        scan_dir(
            &ctx.project_root,
            &ctx.project_root,
            &rules,
            &mut findings,
            &mut files_scanned,
            max_files,
        )?;

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(json!({
            "findings": findings,
            "total_findings": findings.len(),
            "files_scanned": files_scanned,
            "duration_ms": duration_ms,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "max_files": {"type": "integer", "default": 2000}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}

fn is_excluded(name: &str) -> bool {
    matches!(
        name,
        "target"
            | ".git"
            | "node_modules"
            | "dist"
            | "build"
            | ".next"
            | ".venv"
            | "venv"
            | "__pycache__"
    )
}

fn is_text_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
        return false;
    };
    matches!(
        ext,
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "py"
            | "go"
            | "java"
            | "kt"
            | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "cs"
            | "rb"
            | "php"
            | "swift"
            | "toml"
            | "yaml"
            | "yml"
            | "json"
            | "env"
            | "ini"
            | "cfg"
            | "conf"
            | "sh"
            | "bash"
            | "zsh"
            | "ps1"
            | "dockerfile"
            | "md"
    )
}

fn scan_dir(
    root: &Path,
    dir: &Path,
    rules: &[Rule],
    findings: &mut Vec<Value>,
    files_scanned: &mut usize,
    max_files: usize,
) -> Result<(), NexusToolError> {
    if *files_scanned >= max_files {
        return Ok(());
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        if *files_scanned >= max_files {
            break;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if is_excluded(&name) {
            continue;
        }
        if path.is_dir() {
            let _ = scan_dir(root, &path, rules, findings, files_scanned, max_files);
        } else if is_text_file(&path) {
            *files_scanned += 1;
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            // Skip giant files (>1MB)
            if content.len() > 1_000_000 {
                continue;
            }
            for (line_no, line) in content.lines().enumerate() {
                for rule in rules {
                    if let Some(m) = rule.pattern.find(line) {
                        let rel = path.strip_prefix(root).unwrap_or(&path);
                        let preview = m.as_str();
                        let preview_trunc = if preview.len() > 60 {
                            format!("{}...", &preview[..60])
                        } else {
                            preview.to_string()
                        };
                        findings.push(json!({
                            "file": rel.to_string_lossy(),
                            "line": line_no + 1,
                            "rule": rule.name,
                            "preview": preview_trunc,
                        }));
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rule_aws_access_key_matches() {
        let rules = build_rules();
        let aws = rules.iter().find(|r| r.name == "aws_access_key").unwrap();
        assert!(aws.pattern.is_match("let key = \"AKIAIOSFODNN7EXAMPLE\";"));
    }

    #[test]
    fn test_rule_github_token() {
        let rules = build_rules();
        let gh = rules.iter().find(|r| r.name == "github_token").unwrap();
        assert!(gh
            .pattern
            .is_match("token: ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
    }

    #[test]
    fn test_rule_pem_private_key() {
        let rules = build_rules();
        let pem = rules.iter().find(|r| r.name == "private_key_pem").unwrap();
        assert!(pem.pattern.is_match("-----BEGIN RSA PRIVATE KEY-----"));
        assert!(pem.pattern.is_match("-----BEGIN OPENSSH PRIVATE KEY-----"));
    }

    #[test]
    fn test_is_excluded() {
        assert!(is_excluded("target"));
        assert!(is_excluded("node_modules"));
        assert!(!is_excluded("src"));
    }

    #[test]
    fn test_safety_readonly() {
        let s = SecretScanTool.safety();
        assert!(s.read_only && !s.can_execute_subproc);
    }
}
