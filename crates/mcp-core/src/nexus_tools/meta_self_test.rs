//! `other::meta_self_test` — invoke a few read-only handlers as smoke test.
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use crate::nexus_tool_catalog::NexusToolCatalog;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct MetaSelfTestTool;

#[async_trait]
impl NexusToolHandler for MetaSelfTestTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let cat = match NexusToolCatalog::global() {
            Some(c) => c,
            None => return Ok(json!({"ok": false, "error": "catalog not initialized"})),
        };
        // Probe a small set of pure read-only tools.
        let probes = [
            "util_pid",
            "util_uptime",
            "util_hostname",
            "util_cpu_count",
            "rustc_version",
        ];
        let mut results: Vec<Value> = vec![];
        let mut passed = 0usize;
        for name in probes.iter() {
            let r = cat.execute(name, ctx, &json!({})).await;
            let ok = r.is_ok();
            if ok { passed += 1; }
            results.push(json!({
                "tool": name,
                "ok": ok,
                "error": r.err().map(|e| e.to_string()),
            }));
        }
        Ok(json!({
            "ok": true,
            "passed": passed,
            "total": probes.len(),
            "results": results,
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
