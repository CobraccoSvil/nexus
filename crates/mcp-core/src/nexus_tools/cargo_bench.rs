//! `performance::cargo_bench` — wrapper di `cargo bench [-p MEMBER] [filter]`.
//!
//! Esegue i benchmark del progetto. Ritorna l'output testuale (i bench
//! format sono molto variegati: libtest, criterion, iai) e il conteggio
//! aggregato di bench passati/falliti dalla riga summary `test result: ...`
//! quando presente.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoBenchTool;

#[async_trait]
impl NexusToolHandler for CargoBenchTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let workspace_member = args
            .get("workspace_member")
            .and_then(Value::as_str)
            .map(String::from);
        let filter = args.get("filter").and_then(Value::as_str).map(String::from);

        let mut cmd: Vec<String> = vec!["bench".into()];
        if let Some(m) = &workspace_member {
            cmd.push("-p".into());
            cmd.push(m.clone());
        }
        if let Some(f) = &filter {
            cmd.push(f.clone());
        }
        let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        let out = run_cmd("cargo", &refs, &ctx.project_root, ctx.timeout_secs).await?;

        let bench_count = out
            .stdout
            .lines()
            .filter(|l| l.trim_start().starts_with("test ") && l.contains(" bench:"))
            .count();

        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "bench_count": bench_count,
            "stdout": out.stdout,
            "workspace_member": workspace_member,
            "filter": filter,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "workspace_member": {"type": "string"},
                "filter": {"type": "string"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::write_subproc()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_bench_count_heuristic() {
        // Output libtest tipico per bench
        let stdout = "test b1 ... bench:       1,234 ns/iter (+/- 56)\ntest b2 ... bench:       2,345 ns/iter (+/- 12)\n";
        let count = stdout
            .lines()
            .filter(|l| l.trim_start().starts_with("test ") && l.contains(" bench:"))
            .count();
        assert_eq!(count, 2);
    }
}
