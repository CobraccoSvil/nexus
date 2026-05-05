//! `testing::cargo_test` — wrapper di `cargo test [-p MEMBER] [filter]`.
//!
//! Esegue la test suite del progetto e parsa il risultato in una struttura
//! standard: passed/failed/ignored counts e lista dei test falliti con
//! output. Per il parsing usa il flag `--no-fail-fast` così si raccolgono
//! TUTTI i fail in un solo run.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoTestTool;

#[async_trait]
impl NexusToolHandler for CargoTestTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let workspace_member = args
            .get("workspace_member")
            .and_then(Value::as_str)
            .map(String::from);
        let filter = args
            .get("filter")
            .and_then(Value::as_str)
            .map(String::from);
        let release = args.get("release").and_then(Value::as_bool).unwrap_or(false);

        let mut cmd: Vec<String> = vec!["test".into(), "--no-fail-fast".into()];
        if release {
            cmd.push("--release".into());
        }
        if let Some(m) = &workspace_member {
            cmd.push("-p".into());
            cmd.push(m.clone());
        }
        if let Some(f) = &filter {
            cmd.push(f.clone());
        }
        let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        let out = run_cmd("cargo", &refs, &ctx.project_root, ctx.timeout_secs).await?;

        let stats = parse_test_output(&out.stdout);

        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "passed": stats.passed,
            "failed": stats.failed,
            "ignored": stats.ignored,
            "total": stats.passed + stats.failed + stats.ignored,
            "failed_tests": stats.failed_names,
            "workspace_member": workspace_member,
            "filter": filter,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "workspace_member": {"type": "string"},
                "filter": {"type": "string", "description": "Filtro per nome test (substring match)"},
                "release": {"type": "boolean"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::write_subproc()
    }
}

#[derive(Debug, Default)]
struct TestStats {
    passed: usize,
    failed: usize,
    ignored: usize,
    failed_names: Vec<String>,
}

/// Parser per l'output testuale di `cargo test`.
///
/// Esempio di riga summary:
/// `test result: ok. 5 passed; 2 failed; 1 ignored; 0 measured; 0 filtered out`
///
/// Esempio di riga fail:
/// `test module::test_name ... FAILED`
fn parse_test_output(stdout: &str) -> TestStats {
    let mut stats = TestStats::default();
    for line in stdout.lines() {
        let line = line.trim();

        // Riga summary — somma su tutti i binari di test.
        // Formato: "test result: ok. N passed; M failed; K ignored; ..."
        // Oppure:  "test result: FAILED. N passed; M failed; ..."
        // Parsing robusto: scan token-level con pattern "<num> <keyword>",
        // ignorando i verbi iniziali "ok." / "FAILED.".
        if let Some(rest) = line.strip_prefix("test result: ") {
            let normalized = rest.replace(';', " ");
            let mut prev_num: Option<usize> = None;
            for token in normalized.split_whitespace() {
                if let Ok(n) = token.parse::<usize>() {
                    prev_num = Some(n);
                } else if let Some(n) = prev_num.take() {
                    match token {
                        "passed" => stats.passed += n,
                        "failed" => stats.failed += n,
                        "ignored" => stats.ignored += n,
                        _ => {}
                    }
                }
            }
        }
        // Riga di fail individuale
        else if let Some(rest) = line.strip_prefix("test ") {
            if rest.ends_with("... FAILED") {
                let name = rest.trim_end_matches(" ... FAILED").to_string();
                if !name.is_empty() {
                    stats.failed_names.push(name);
                }
            }
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_output_summary() {
        let out = "test module::t1 ... ok\ntest module::t2 ... FAILED\n\ntest result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n";
        let s = parse_test_output(out);
        assert_eq!(s.passed, 1);
        assert_eq!(s.failed, 1);
        assert_eq!(s.ignored, 0);
        assert_eq!(s.failed_names, vec!["module::t2".to_string()]);
    }

    #[test]
    fn test_parse_multiple_summaries() {
        // Due binari di test: i numeri si devono sommare
        let out = "test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n\ntest result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out\n";
        let s = parse_test_output(out);
        assert_eq!(s.passed, 5);
        assert_eq!(s.ignored, 1);
    }
}
