//! `github::gh_issue_list` — wrapper di `gh issue list` per elencare issue
//! di un repo GitHub.
//!
//! Richiede il binario `gh` installato e autenticato sul sistema. Se `gh`
//! manca o non è autenticato, l'handler ritorna un errore strutturato invece
//! di panicare.
//!
//! Input:
//! - `repo` (optional): `"owner/name"`. Se omesso, `gh` usa il repo del cwd.
//! - `state` (optional): `"open"` | `"closed"` | `"all"` (default `"open"`)
//! - `limit` (optional): max issue ritornate (default 30)
//! - `label` (optional): filtra per label
//! - `author` (optional): filtra per author
//!
//! Output: JSON array parsato da `gh issue list --json`.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GhIssueListTool;

#[async_trait]
impl NexusToolHandler for GhIssueListTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let state = args
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("open")
            .to_string();
        // Whitelist di stati accettati per evitare injection nei flag
        if !matches!(state.as_str(), "open" | "closed" | "all") {
            return Err(NexusToolError::BadInput(
                "state deve essere 'open', 'closed' o 'all'".into(),
            ));
        }
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(30)
            .min(200)
            .to_string();
        let repo = args.get("repo").and_then(Value::as_str).map(String::from);
        let label = args.get("label").and_then(Value::as_str).map(String::from);
        let author = args.get("author").and_then(Value::as_str).map(String::from);

        // Campi richiesti dall'output JSON di `gh`
        let json_fields =
            "number,title,state,author,labels,createdAt,updatedAt,assignees,comments,url";

        let mut cmd: Vec<String> = vec![
            "issue".into(),
            "list".into(),
            "--state".into(),
            state,
            "--limit".into(),
            limit,
            "--json".into(),
            json_fields.to_string(),
        ];
        if let Some(r) = repo {
            cmd.push("--repo".into());
            cmd.push(r);
        }
        if let Some(l) = label {
            cmd.push("--label".into());
            cmd.push(l);
        }
        if let Some(a) = author {
            cmd.push("--author".into());
            cmd.push(a);
        }
        let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        let out = run_cmd("gh", &refs, &ctx.project_root, ctx.timeout_secs).await?;

        if !out.success() {
            return Ok(json!({
                "ok": false,
                "exit_code": out.exit_code,
                "duration_ms": out.duration_ms,
                "error": out.stderr.trim().to_string(),
                "hint": "Verifica che `gh` sia installato e autenticato (`gh auth status`).",
                "issues": [],
            }));
        }

        // Parsing: `gh --json` emette un JSON array in stdout
        let issues: Value = match serde_json::from_str::<Value>(&out.stdout) {
            Ok(v) => v,
            Err(_) => json!([]),
        };
        let count = issues.as_array().map(|a| a.len()).unwrap_or(0);

        Ok(json!({
            "ok": true,
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "count": count,
            "issues": issues,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo": {"type": "string", "description": "owner/name — omesso usa il repo del cwd"},
                "state": {"type": "string", "enum": ["open", "closed", "all"]},
                "limit": {"type": "integer", "description": "Max issue (default 30, max 200)"},
                "label": {"type": "string"},
                "author": {"type": "string"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        // Fa egress HTTPS all'API GitHub via `gh`.
        NexusToolSafety {
            read_only: true,
            can_write_filesystem: false,
            can_execute_subproc: true,
            network_egress: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety_has_network_egress() {
        let s = GhIssueListTool.safety();
        assert!(s.network_egress);
        assert!(s.can_execute_subproc);
        assert!(!s.can_write_filesystem);
    }

    #[test]
    fn test_input_schema_accepts_state_enum() {
        let schema = GhIssueListTool.input_schema();
        let state = schema["properties"]["state"]["enum"].as_array().unwrap();
        assert!(state.iter().any(|v| v == "open"));
        assert!(state.iter().any(|v| v == "closed"));
        assert!(state.iter().any(|v| v == "all"));
    }
}
