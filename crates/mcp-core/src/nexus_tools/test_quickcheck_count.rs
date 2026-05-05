//! `testing::test_quickcheck_count` — conta uso di quickcheck.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct TestQuickcheckCountTool;

#[async_trait]
impl NexusToolHandler for TestQuickcheckCountTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &["#[quickcheck]", "quickcheck!", "use quickcheck"]);
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "attribute": counts[0],
            "macro": counts[1],
            "imports": counts[2],
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
