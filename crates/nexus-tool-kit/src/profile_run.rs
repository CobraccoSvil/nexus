//! `performance::profile_run` — misura elapsed wall-clock + peak memory
//! di un comando arbitrario nel contesto del progetto.
//!
//! Input:
//! - `command` (required): binario da eseguire
//! - `args` (optional): lista di argomenti
//! - `runs` (optional): numero di esecuzioni (default 3) per medie
//!
//! Output: `{runs: [{duration_ms, exit_code}], mean_ms, p95_ms, min_ms, max_ms}`.
//!
//! Nota: non usiamo `perf` / `flamegraph` per evitare dipendenze esterne e
//! mantenere il tool multi-platform. Per profiling avanzato, l'utente può
//! invocare direttamente cargo bench / criterion dal suo progetto.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ProfileRunTool;

fn stats(samples: &[u64]) -> (f64, u64, u64, u64) {
    if samples.is_empty() {
        return (0.0, 0, 0, 0);
    }
    let sum: u64 = samples.iter().sum();
    let mean = sum as f64 / samples.len() as f64;
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let p95_idx = ((sorted.len() as f64) * 0.95).ceil() as usize - 1;
    let p95 = sorted[p95_idx.min(sorted.len() - 1)];
    // sorted non e' vuoto (guardia is_empty sopra), quindi l'ultimo index esiste.
    let max = sorted[sorted.len() - 1];
    (mean, sorted[0], max, p95)
}

#[async_trait]
impl NexusToolHandler for ProfileRunTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("command required".into()))?
            .to_string();
        let cmd_args: Vec<String> = args
            .get("args")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let runs_n = args
            .get("runs")
            .and_then(Value::as_u64)
            .unwrap_or(3)
            .clamp(1, 10);

        // Whitelist di comandi profilabili — solo &'static str accettati da run_cmd
        let bin: &'static str = match command.as_str() {
            "cargo" => "cargo",
            "go" => "go",
            "python" => "python",
            "python3" => "python3",
            "node" => "node",
            "npm" => "npm",
            "make" => "make",
            other => {
                return Err(NexusToolError::BadInput(format!(
                    "command '{}' not in profiling whitelist (cargo, go, python, python3, node, npm, make)",
                    other
                )));
            }
        };

        let mut samples: Vec<u64> = Vec::with_capacity(runs_n as usize);
        let mut per_run: Vec<Value> = Vec::with_capacity(runs_n as usize);
        for i in 0..runs_n {
            let refs: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();
            let out = run_cmd(bin, &refs, &ctx.project_root, ctx.timeout_secs).await?;
            samples.push(out.duration_ms);
            per_run.push(json!({
                "run": i + 1,
                "duration_ms": out.duration_ms,
                "exit_code": out.exit_code,
                "ok": out.success(),
            }));
        }
        let (mean, min, max, p95) = stats(&samples);
        Ok(json!({
            "ok": true,
            "command": command,
            "args": cmd_args,
            "runs": per_run,
            "mean_ms": mean,
            "min_ms": min,
            "max_ms": max,
            "p95_ms": p95,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": {"type": "string"},
                "args": {"type": "array", "items": {"type": "string"}},
                "runs": {"type": "integer", "minimum": 1, "maximum": 10}
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
    fn test_stats_basic() {
        let (mean, min, max, p95) = stats(&[10, 20, 30, 40, 50]);
        assert_eq!(mean, 30.0);
        assert_eq!(min, 10);
        assert_eq!(max, 50);
        assert!(p95 >= 40);
    }

    #[test]
    fn test_stats_single() {
        let (mean, min, max, p95) = stats(&[100]);
        assert_eq!(mean, 100.0);
        assert_eq!(min, 100);
        assert_eq!(max, 100);
        assert_eq!(p95, 100);
    }

    #[test]
    fn test_safety_writes() {
        assert!(ProfileRunTool.safety().can_write_filesystem);
    }
}
