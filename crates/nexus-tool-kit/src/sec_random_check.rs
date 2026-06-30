//! `security::sec_random_check` — find non-secure RNG usage.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SecRandomCheckTool;

#[async_trait]
impl NexusToolHandler for SecRandomCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &[
                "rand::thread_rng",
                "rand::random",
                "SmallRng",
                "StdRng",
                "OsRng",
                "getrandom",
                "ring::rand",
            ],
        );
        let insecure = counts[0] + counts[1] + counts[2];
        let secure = counts[3] + counts[4] + counts[5] + counts[6];
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "thread_rng": counts[0],
            "rand_random": counts[1],
            "small_rng": counts[2],
            "std_rng": counts[3],
            "os_rng": counts[4],
            "getrandom": counts[5],
            "ring_rand": counts[6],
            "non_crypto_total": insecure,
            "crypto_total": secure,
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}
