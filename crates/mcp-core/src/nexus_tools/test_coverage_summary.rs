//! `testing::test_coverage_summary` — legge cobertura.xml/lcov.info se esistono.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct TestCoverageSummaryTool;

#[async_trait]
impl NexusToolHandler for TestCoverageSummaryTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let candidates = [
            "cobertura.xml",
            "tarpaulin-report.html",
            "lcov.info",
            "target/llvm-cov-target/lcov.info",
            "target/coverage/lcov.info",
        ];
        let mut found: Vec<Value> = vec![];
        for c in &candidates {
            let p = ctx.project_root.join(c);
            if p.is_file() {
                let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                found.push(json!({"path": c, "size": size}));
            }
        }
        Ok(json!({"ok": true, "found": found.len(), "reports": found}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
