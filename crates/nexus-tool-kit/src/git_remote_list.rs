//! `vcs::git_remote_list` — wrapper di `git remote -v`.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;

pub struct GitRemoteListTool;

#[async_trait]
impl NexusToolHandler for GitRemoteListTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let out = run_cmd(
            "git",
            &["remote", "-v"],
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

        // Layout: "origin\thttps://...\t(fetch)"  oppure "(push)"
        let mut by_name: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
        for line in out.stdout.lines() {
            let mut parts = line.split('\t');
            let name = parts.next().unwrap_or("").to_string();
            let rest = parts.next().unwrap_or("");
            let mut inner = rest.rsplitn(2, ' ');
            let kind = inner
                .next()
                .unwrap_or("")
                .trim_start_matches('(')
                .trim_end_matches(')');
            let url = inner.next().unwrap_or("");
            if name.is_empty() || url.is_empty() {
                continue;
            }
            let entry = by_name.entry(name).or_insert((None, None));
            match kind {
                "fetch" => entry.0 = Some(url.to_string()),
                "push" => entry.1 = Some(url.to_string()),
                _ => {}
            }
        }

        let remotes: Vec<Value> = by_name
            .into_iter()
            .map(|(name, (fetch, push))| json!({"name": name, "fetch": fetch, "push": push}))
            .collect();

        Ok(json!({
            "ok": true,
            "count": remotes.len(),
            "remotes": remotes,
            "duration_ms": out.duration_ms,
        }))
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_safety() {
        assert!(GitRemoteListTool.safety().read_only);
    }
}
