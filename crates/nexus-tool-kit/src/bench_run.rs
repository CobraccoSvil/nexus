//! `performance::bench_run` — dispatcher generico per benchmark runs.
//!
//! Strategia:
//! - `Cargo.toml` → `cargo bench [filter]`
//! - `package.json` con script `bench` → `npm run bench`
//!
//! Ritorna stdout/exit_code/duration + conteggio "bench" presente in output
//! (riuso parser leggero di CargoBenchTool).

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct BenchRunTool;

fn count_bench_lines(stdout: &str) -> usize {
    stdout
        .lines()
        .filter(|l| l.contains("bench:") || l.contains("test ") && l.contains("bench"))
        .count()
}

#[async_trait]
impl NexusToolHandler for BenchRunTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let filter = args.get("filter").and_then(Value::as_str).map(String::from);

        let cargo_toml = ctx.project_root.join("Cargo.toml");
        let package_json = ctx.project_root.join("package.json");

        if cargo_toml.is_file() {
            let mut cmd: Vec<String> = vec!["bench".into()];
            if let Some(f) = &filter {
                cmd.push(f.clone());
            }
            let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
            let out = run_cmd("cargo", &refs, &ctx.project_root, ctx.timeout_secs).await?;
            return Ok(json!({
                "ok": out.success(),
                "stack": "rust",
                "exit_code": out.exit_code,
                "duration_ms": out.duration_ms,
                "bench_count": count_bench_lines(&out.stdout),
                "stdout": out.stdout,
                "stderr": out.stderr,
                "filter": filter,
            }));
        }

        if package_json.is_file() {
            let content = std::fs::read_to_string(&package_json).unwrap_or_default();
            let v: Value = serde_json::from_str(&content).unwrap_or(Value::Null);
            let has_bench = v
                .get("scripts")
                .and_then(|s| s.get("bench"))
                .and_then(Value::as_str)
                .is_some();
            if !has_bench {
                return Ok(json!({
                    "ok": false,
                    "stack": "node",
                    "error": "package.json has no 'bench' script defined",
                }));
            }
            // Punto unico in nexus_tools::run_npm_script_node_stack (regola L, S65).
            return super::run_npm_script_node_stack(ctx, "bench").await;
        }

        Ok(json!({
            "ok": false,
            "stack": "unknown",
            "error": "No supported project manifest found (Cargo.toml, package.json)",
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "filter": {"type": "string", "description": "Benchmark name filter (cargo)"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::write_subproc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_bench_lines() {
        let stdout = "test fast_bench ... bench: 1,234 ns/iter (+/- 100)\ntest slow_bench ... bench: 5,678 ns/iter (+/- 500)\nok\n";
        assert_eq!(count_bench_lines(stdout), 2);
    }

    #[test]
    fn test_safety_writes() {
        assert!(BenchRunTool.safety().can_write_filesystem);
    }
}
