//! `security::sec_eval_check` — heuristic scan for eval-like patterns.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SecEvalCheckTool;

#[async_trait]
impl NexusToolHandler for SecEvalCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &[
                "rhai::Engine",
                "mlua",
                "rlua",
                "wasmtime::",
                "wasmer::",
                "deno_core",
            ],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "rhai": counts[0],
            "mlua": counts[1],
            "rlua": counts[2],
            "wasmtime": counts[3],
            "wasmer": counts[4],
            "deno_core": counts[5],
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
