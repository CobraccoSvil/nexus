//! `security::sec_tls_check` — find TLS verify=false / accept_invalid_certs.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SecTlsCheckTool;

#[async_trait]
impl NexusToolHandler for SecTlsCheckTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &[
                "danger_accept_invalid_certs",
                "danger_accept_invalid_hostnames",
                "ServerCertVerifier",
                "rustls",
                "native-tls",
                "InsecureSkipVerify",
            ],
        );
        let danger = counts[0] + counts[1] + counts[5];
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "accept_invalid_certs": counts[0],
            "accept_invalid_hostnames": counts[1],
            "custom_verifier": counts[2],
            "rustls": counts[3],
            "native_tls": counts[4],
            "insecure_skip_verify": counts[5],
            "danger_total": danger,
            "warning": danger > 0,
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
