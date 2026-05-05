//! `security::sec_audit_summary` — high-level overview combining several scans.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SecAuditSummaryTool;

#[async_trait]
impl NexusToolHandler for SecAuditSummaryTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &[
                ".unwrap()",
                "panic!(",
                "unsafe {",
                "unsafe fn",
                "format!(\"SELECT",
                "danger_accept_invalid_certs",
                "Algorithm::None",
                "http://",
            ],
        );
        let mut score = 100i32;
        let mut findings: Vec<&str> = vec![];
        if counts[0] > 50 { score -= 5; findings.push("many_unwrap"); }
        if counts[1] > 5 { score -= 5; findings.push("explicit_panics"); }
        if counts[2] + counts[3] > 0 { score -= 10; findings.push("unsafe_code"); }
        if counts[4] > 0 { score -= 20; findings.push("sql_format_interp"); }
        if counts[5] > 0 { score -= 25; findings.push("tls_disabled"); }
        if counts[6] > 0 { score -= 25; findings.push("jwt_alg_none"); }
        if counts[7] > 5 { score -= 5; findings.push("plaintext_http"); }
        if score < 0 { score = 0; }
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "score": score,
            "findings": findings,
            "details": {
                "unwrap": counts[0],
                "panic": counts[1],
                "unsafe_block": counts[2],
                "unsafe_fn": counts[3],
                "sql_format": counts[4],
                "tls_invalid_certs": counts[5],
                "jwt_alg_none": counts[6],
                "http_urls": counts[7],
            }
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
