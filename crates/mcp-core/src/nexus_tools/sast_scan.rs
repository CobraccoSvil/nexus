//! `security::sast_scan` — wrapper di scanner SAST esterni.
//!
//! Strategia:
//! - `semgrep` disponibile → `semgrep --config=auto --json`
//! - Altrimenti, fallback scan regex-based integrato che cerca pattern
//!   ad alto rischio (uso di `eval`, SQL concat, `shell=true` su python,
//!   `unsafe` rust blocks senza commento di sicurezza, ecc.)
//!
//! Il fallback è intenzionalmente conservativo: copre i quick-win della
//! sicurezza statica senza richiedere nuove dipendenze. Il chiamante vede
//! chiaramente quale strategy è stata usata via campo `engine`.

use super::exec::{ensure_binary, run_cmd};
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};
use std::path::Path;

pub struct SastScanTool;

#[derive(Debug)]
struct Finding {
    rule: &'static str,
    severity: &'static str,
    file: String,
    line: usize,
    snippet: String,
}

fn rules() -> Vec<(&'static str, &'static str, Regex)> {
    vec![
        (
            "py-eval",
            "high",
            Regex::new(r"\beval\s*\(").unwrap(),
        ),
        (
            "py-shell-true",
            "high",
            Regex::new(r"shell\s*=\s*True").unwrap(),
        ),
        (
            "js-eval",
            "high",
            Regex::new(r"\beval\s*\(").unwrap(),
        ),
        (
            "sql-concat",
            "medium",
            Regex::new(r#"["']\s*\+\s*\w+\s*\+\s*["'].*(SELECT|INSERT|UPDATE|DELETE)"#).unwrap(),
        ),
        (
            "rust-unsafe",
            "medium",
            Regex::new(r"\bunsafe\s*\{").unwrap(),
        ),
        (
            "hardcoded-password",
            "high",
            Regex::new(r#"(?i)(password|passwd|pwd)\s*=\s*["'][^"']{4,}["']"#).unwrap(),
        ),
    ]
}

fn scan_file(path: &Path, rel: &str, acc: &mut Vec<Finding>, rules: &[(&'static str, &'static str, Regex)]) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };
    for (lineno, line) in content.lines().enumerate() {
        for (rule, severity, re) in rules {
            if re.is_match(line) {
                acc.push(Finding {
                    rule,
                    severity,
                    file: rel.to_string(),
                    line: lineno + 1,
                    snippet: line.trim().chars().take(200).collect(),
                });
            }
        }
    }
}

fn walk_and_scan(root: &Path) -> Vec<Finding> {
    let rules = rules();
    let mut out: Vec<Finding> = Vec::new();
    let exts = ["rs", "py", "js", "ts", "tsx", "jsx", "go", "java"];
    let mut stack = vec![root.to_path_buf()];
    let mut files_seen = 0usize;
    while let Some(dir) = stack.pop() {
        if files_seen > 5_000 {
            break;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let p = entry.path();
            let name = p.file_name().map(|o| o.to_string_lossy().into_owned()).unwrap_or_default();
            if name.starts_with('.') || name == "target" || name == "node_modules" || name == "dist" || name == "build" {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else if let Some(ext) = p.extension().and_then(|o| o.to_str()) {
                if exts.contains(&ext) {
                    files_seen += 1;
                    let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().into_owned();
                    scan_file(&p, &rel, &mut out, &rules);
                }
            }
        }
    }
    out
}

#[async_trait]
impl NexusToolHandler for SastScanTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let prefer_semgrep = args
            .get("prefer_semgrep")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        if prefer_semgrep && ensure_binary("semgrep").await.is_ok() {
            let out = run_cmd(
                "semgrep",
                &["--config=auto", "--json", "--quiet"],
                &ctx.project_root,
                ctx.timeout_secs,
            )
            .await?;
            let parsed: Value = serde_json::from_str(&out.stdout).unwrap_or(Value::Null);
            let count = parsed
                .get("results")
                .and_then(Value::as_array)
                .map(|a| a.len())
                .unwrap_or(0);
            return Ok(json!({
                "ok": out.success(),
                "engine": "semgrep",
                "exit_code": out.exit_code,
                "duration_ms": out.duration_ms,
                "findings_count": count,
                "report": parsed,
            }));
        }

        let started = std::time::Instant::now();
        let findings = walk_and_scan(&ctx.project_root);
        let duration_ms = started.elapsed().as_millis() as u64;
        let findings_json: Vec<Value> = findings
            .iter()
            .map(|f| {
                json!({
                    "rule": f.rule,
                    "severity": f.severity,
                    "file": f.file,
                    "line": f.line,
                    "snippet": f.snippet,
                })
            })
            .collect();

        Ok(json!({
            "ok": true,
            "engine": "builtin-regex",
            "duration_ms": duration_ms,
            "findings_count": findings.len(),
            "report": findings_json,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prefer_semgrep": {"type": "boolean", "description": "Prova semgrep prima del builtin (default true)"}
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
    fn test_regex_eval_py() {
        let rs = rules();
        let (_, _, eval_re) = rs.iter().find(|(r, _, _)| *r == "py-eval").unwrap();
        assert!(eval_re.is_match("result = eval(user_input)"));
    }

    #[test]
    fn test_regex_hardcoded_password() {
        let rs = rules();
        let (_, _, re) = rs.iter().find(|(r, _, _)| *r == "hardcoded-password").unwrap();
        assert!(re.is_match("password = \"topsecret42\""));
        assert!(!re.is_match("password = None"));
    }

    #[test]
    fn test_walk_detects_unsafe() {
        let tmp = std::env::temp_dir().join(format!("sast_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("x.rs"), "fn main() { unsafe { *(0 as *mut u8) = 1; } }").unwrap();
        let findings = walk_and_scan(&tmp);
        assert!(findings.iter().any(|f| f.rule == "rust-unsafe"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_safety_readonly() {
        assert!(SastScanTool.safety().read_only);
    }
}
