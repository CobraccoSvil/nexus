//! `dependencies::cargo_search` — `cargo search <query> --limit N` (network).
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoSearchTool;

#[async_trait]
impl NexusToolHandler for CargoSearchTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("query required".into()))?;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(10)
            .min(100);
        let limit_str = limit.to_string();
        let out = run_cmd(
            "cargo",
            &["search", query, "--limit", &limit_str],
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
        let mut results = Vec::new();
        for line in out.stdout.lines() {
            // format: name = "1.2.3"    # description
            if let Some(eq) = line.find(" = ") {
                let name = line[..eq].trim().to_string();
                let rest = &line[eq + 3..];
                let version = rest.split('"').nth(1).unwrap_or("").to_string();
                let desc = rest.split('#').nth(1).unwrap_or("").trim().to_string();
                results.push(json!({"name": name, "version": version, "description": desc}));
            }
        }
        Ok(
            json!({"ok": true, "query": query, "count": results.len(), "results": results, "duration_ms": out.duration_ms}),
        )
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["query"],"properties":{"query":{"type":"string"},"limit":{"type":"integer"}}})
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: true,
            can_write_filesystem: false,
            can_execute_subproc: true,
            network_egress: true,
        }
    }
}
