//! `security::sec_jwt_secret_check` — find hardcoded JWT secrets and weak algos.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SecJwtSecretCheckTool;

#[async_trait]
impl NexusToolHandler for SecJwtSecretCheckTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &[
                "jsonwebtoken",
                "jwt_secret",
                "JWT_SECRET",
                "Algorithm::HS256",
                "Algorithm::None",
                "verify = false",
            ],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "jsonwebtoken_dep": counts[0],
            "jwt_secret_lower": counts[1],
            "jwt_secret_upper": counts[2],
            "alg_hs256": counts[3],
            "alg_none": counts[4],
            "verify_false": counts[5],
            "warning": counts[4] > 0 || counts[5] > 0,
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
