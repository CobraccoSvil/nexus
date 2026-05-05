//! `security::sec_env_var_check` — count `std::env::var` and default fallbacks.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SecEnvVarCheckTool;

#[async_trait]
impl NexusToolHandler for SecEnvVarCheckTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &["std::env::var", "env::var(", ".unwrap_or_else(|_|", ".unwrap_or(\""],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "std_env_var": counts[0],
            "env_var_call": counts[1],
            "unwrap_or_else_closure": counts[2],
            "unwrap_or_string_default": counts[3],
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
