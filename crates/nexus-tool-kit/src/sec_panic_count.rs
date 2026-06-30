//! `security::sec_panic_count` — count panic-inducing macros.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SecPanicCountTool;

#[async_trait]
impl NexusToolHandler for SecPanicCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &["panic!(", "todo!(", "unimplemented!(", "unreachable!("],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "panic": counts[0],
            "todo": counts[1],
            "unimplemented": counts[2],
            "unreachable": counts[3],
            "total": counts.iter().sum::<usize>(),
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
