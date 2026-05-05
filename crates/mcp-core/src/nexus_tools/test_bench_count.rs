//! `testing::test_bench_count` — conta `#[bench]` e benchmark Criterion.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct TestBenchCountTool;

#[async_trait]
impl NexusToolHandler for TestBenchCountTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(&ctx.project_root, &["#[bench]", "criterion_group!", "criterion_main!"]);
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "bench_attr": counts[0],
            "criterion_group": counts[1],
            "criterion_main": counts[2],
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
