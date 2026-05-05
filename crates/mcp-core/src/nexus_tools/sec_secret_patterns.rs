//! `security::sec_secret_patterns` — heuristic scan for hardcoded secrets.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SecSecretPatternsTool;

#[async_trait]
impl NexusToolHandler for SecSecretPatternsTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &[
                "api_key", "apikey", "API_KEY",
                "secret", "SECRET",
                "password", "PASSWORD",
                "token", "TOKEN",
                "AKIA", // AWS key prefix
                "BEGIN PRIVATE KEY",
            ],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "api_key_lower": counts[0],
            "apikey": counts[1],
            "api_key_upper": counts[2],
            "secret_lower": counts[3],
            "secret_upper": counts[4],
            "password_lower": counts[5],
            "password_upper": counts[6],
            "token_lower": counts[7],
            "token_upper": counts[8],
            "aws_key_prefix": counts[9],
            "private_key_pem": counts[10],
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
