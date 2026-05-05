//! `build::build_rerun_checks` — count `cargo:rerun-if-` directives in build scripts.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct BuildRerunChecksTool;

#[async_trait]
impl NexusToolHandler for BuildRerunChecksTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &["cargo:rerun-if-changed", "cargo:rerun-if-env-changed", "cargo:rustc-link-", "cargo:warning="],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "rerun_if_changed": counts[0],
            "rerun_if_env_changed": counts[1],
            "rustc_link": counts[2],
            "warnings": counts[3],
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
