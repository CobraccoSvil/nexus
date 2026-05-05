//! `documentation::doc_license_detect` — rileva file LICENSE e tipo (heuristic).
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DocLicenseDetectTool;

fn detect_license(text: &str) -> &'static str {
    let lower = text.to_lowercase();
    if lower.contains("mit license") { "MIT" }
    else if lower.contains("apache license") && lower.contains("version 2.0") { "Apache-2.0" }
    else if lower.contains("gnu general public license") { "GPL" }
    else if lower.contains("bsd") && lower.contains("license") { "BSD" }
    else if lower.contains("mozilla public license") { "MPL" }
    else if lower.contains("the unlicense") || lower.contains("unlicense") { "Unlicense" }
    else { "unknown" }
}

#[async_trait]
impl NexusToolHandler for DocLicenseDetectTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let candidates = ["LICENSE", "LICENSE.md", "LICENSE.txt", "COPYING", "LICENCE"];
        for c in &candidates {
            let p = ctx.project_root.join(c);
            if p.is_file() {
                let text = std::fs::read_to_string(&p).unwrap_or_default();
                return Ok(json!({
                    "ok": true,
                    "exists": true,
                    "filename": c,
                    "size": text.len(),
                    "license": detect_license(&text),
                }));
            }
        }
        Ok(json!({"ok": true, "exists": false, "license": "none"}))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
