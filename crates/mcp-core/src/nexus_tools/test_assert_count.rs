//! `testing::test_assert_count` — conta assert!/assert_eq!/assert_ne!.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct TestAssertCountTool;

#[async_trait]
impl NexusToolHandler for TestAssertCountTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &["assert!(", "assert_eq!(", "assert_ne!(", "debug_assert"]);
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "assert": counts[0],
            "assert_eq": counts[1],
            "assert_ne": counts[2],
            "debug_assert": counts[3],
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
