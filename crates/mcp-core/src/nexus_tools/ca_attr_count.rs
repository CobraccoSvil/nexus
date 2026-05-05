//! `code_analysis::ca_attr_count` — count common attribute macros.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CaAttrCountTool;

#[async_trait]
impl NexusToolHandler for CaAttrCountTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &["#[", "#![", "#[allow(", "#[deny(", "#[warn(", "#[deprecated", "#[inline"],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "outer_attr": counts[0],
            "inner_attr": counts[1],
            "allow": counts[2],
            "deny": counts[3],
            "warn": counts[4],
            "deprecated": counts[5],
            "inline": counts[6],
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
