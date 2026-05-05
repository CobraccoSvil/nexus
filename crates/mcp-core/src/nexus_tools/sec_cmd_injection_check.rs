//! `security::sec_cmd_injection_check` — find Command::new + shell -c patterns.
use super::perf_scan::scan_substrings;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SecCmdInjectionCheckTool;

#[async_trait]
impl NexusToolHandler for SecCmdInjectionCheckTool {
    async fn execute(&self, ctx: &NexusToolContext, _args: &Value) -> Result<Value, NexusToolError> {
        let (counts, files) = scan_substrings(
            &ctx.project_root,
            &[
                "Command::new(\"sh\")",
                "Command::new(\"bash\")",
                "Command::new(\"cmd\")",
                "Command::new(\"powershell\")",
                ".arg(\"-c\")",
                "process::Command::new",
                "tokio::process::Command",
            ],
        );
        Ok(json!({
            "ok": true,
            "files_scanned": files,
            "sh": counts[0],
            "bash": counts[1],
            "cmd": counts[2],
            "powershell": counts[3],
            "arg_dash_c": counts[4],
            "process_command": counts[5],
            "tokio_command": counts[6],
            "warning": counts[0] + counts[1] + counts[2] + counts[3] > 0,
        }))
    }
    fn safety(&self) -> NexusToolSafety { NexusToolSafety::read_only() }
}
