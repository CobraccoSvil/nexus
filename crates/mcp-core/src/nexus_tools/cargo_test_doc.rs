//! `testing::cargo_test_doc` — `cargo test --doc`.
use super::{run_cargo_test_subset, NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::Value;

pub struct CargoTestDocTool;

#[async_trait]
impl NexusToolHandler for CargoTestDocTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        run_cargo_test_subset(ctx, "--doc").await
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::write_subproc()
    }
}
