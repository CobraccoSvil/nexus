//! `security::sec_localhost_count` — count localhost / 127.0.0.1 / 0.0.0.0 references.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SecLocalhostCountTool;

#[async_trait]
impl NexusToolHandler for SecLocalhostCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &["localhost", "127.0.0.1", "0.0.0.0", "::1"],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "localhost": counts[0],
            "ipv4_loopback": counts[1],
            "ipv4_any": counts[2],
            "ipv6_loopback": counts[3],
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
