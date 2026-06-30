//! `testing::test_proptest_count` — conta uso di `proptest!` e `prop_assert`.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct TestProptestCountTool;

#[async_trait]
impl NexusToolHandler for TestProptestCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &["proptest!", "prop_assert", "use proptest"],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "proptest_macro": counts[0],
            "prop_assert": counts[1],
            "imports": counts[2],
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
