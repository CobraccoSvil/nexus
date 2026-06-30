//! `security::sec_http_url_count` — count plaintext http:// vs https:// URLs.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SecHttpUrlCountTool;

#[async_trait]
impl NexusToolHandler for SecHttpUrlCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &["http://", "https://"]);
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "http": counts[0],
            "https": counts[1],
            "ratio_plaintext": if counts[0] + counts[1] == 0 { 0.0 } else { counts[0] as f64 / (counts[0] + counts[1]) as f64 },
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
