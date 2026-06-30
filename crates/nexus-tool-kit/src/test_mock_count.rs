//! `testing::test_mock_count` — conta uso di mock libraries (mockall, mockito, ...).
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct TestMockCountTool;

#[async_trait]
impl NexusToolHandler for TestMockCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &["use mockall", "mock!", "MockServer", "wiremock"],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "mockall_imports": counts[0],
            "mock_macro": counts[1],
            "mock_server": counts[2],
            "wiremock": counts[3],
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
