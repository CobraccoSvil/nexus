//! `vcs::git_gc_dry` — invoca `git gc --auto --prune=never` (rispetta gc.auto).
use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GitGcDryTool;

#[async_trait]
impl NexusToolHandler for GitGcDryTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        // count-objects -v: indica se gc è "needed"
        let out = run_cmd(
            "git",
            &["count-objects", "-v"],
            &ctx.project_root,
            ctx.timeout_secs,
        )
        .await?;
        if !out.success() {
            return Err(NexusToolError::Exec {
                exit_code: out.exit_code,
                stderr: out.stderr,
            });
        }
        let mut loose: u64 = 0;
        let mut packs: u64 = 0;
        for l in out.stdout.lines() {
            if let Some((k, v)) = l.split_once(": ") {
                let n: u64 = v.trim().parse().unwrap_or(0);
                match k {
                    "count" => loose = n,
                    "in-pack" => packs = n,
                    _ => {}
                }
            }
        }
        let needed = loose > 6700;
        Ok(json!({"ok": true, "loose_objects": loose, "in_pack": packs, "gc_needed": needed}))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}
