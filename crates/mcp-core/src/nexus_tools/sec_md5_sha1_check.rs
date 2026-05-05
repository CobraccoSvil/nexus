//! `security::sec_md5_sha1_check` — find weak hash algorithms.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SecMd5Sha1CheckTool;

#[async_trait]
impl NexusToolHandler for SecMd5Sha1CheckTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &["use md5", "Md5::", "use sha1", "Sha1::", "use sha2::Sha256", "Sha256::", "blake3"],
        );
        let weak = counts[0] + counts[1] + counts[2] + counts[3];
        let strong = counts[4] + counts[5] + counts[6];
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "md5_use": counts[0],
            "md5_call": counts[1],
            "sha1_use": counts[2],
            "sha1_call": counts[3],
            "sha256_use": counts[4],
            "sha256_call": counts[5],
            "blake3": counts[6],
            "weak_total": weak,
            "strong_total": strong,
            "warning": weak > 0,
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
