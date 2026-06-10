//! `testing::cargo_test_lib` — `cargo test --lib`.
use super::{
    run_cargo_test_subset, NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety,
};
use async_trait::async_trait;
use serde_json::Value;

pub struct CargoTestLibTool;

#[async_trait]
impl NexusToolHandler for CargoTestLibTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        run_cargo_test_subset(ctx, "--lib").await
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::write_subproc()
    }
}
