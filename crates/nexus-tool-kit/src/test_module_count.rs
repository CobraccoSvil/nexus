//! `testing::test_module_count` — conta moduli `mod tests` con #[cfg(test)].
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct TestModuleCountTool;

#[async_trait]
impl NexusToolHandler for TestModuleCountTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &["mod tests {", "mod tests;", "#[cfg(test)]"],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "mod_tests_inline": counts[0],
            "mod_tests_decl": counts[1],
            "cfg_test": counts[2],
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
