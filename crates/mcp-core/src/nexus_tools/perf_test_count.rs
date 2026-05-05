//! `performance::perf_test_count` — conta `#[test]` e `#[tokio::test]` nei .rs.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct PerfTestCountTool;

#[async_trait]
impl NexusToolHandler for PerfTestCountTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &["#[test]", "#[tokio::test]"]);
        Ok(json!({"ok": true, "files_scanned": files, "test": counts[0], "tokio_test": counts[1], "total": counts[0] + counts[1]}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
