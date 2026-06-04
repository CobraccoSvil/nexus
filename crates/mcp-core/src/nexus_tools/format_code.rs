//! `code_quality::format_code` — wrapper di `cargo fmt`.
//!
//! Default: `cargo fmt --check` (readonly: riporta i file che andrebbero
//! riformattati ma non modifica nulla). Con `apply=true` esegue la
//! formattazione vera e propria.
//!
//! Output:
//! - `ok`: true se non c'è niente da formattare (o apply=true e successo)
//! - `files_changed`: lista dei file che sono stati (o sarebbero) modificati
//!
//! Nota: rustfmt in modalità `--check` esce con code 1 quando ci sono
//! differenze. Non è un errore per noi — lo riportiamo come `ok=false`.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct FormatCodeTool;

#[async_trait]
impl NexusToolHandler for FormatCodeTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let apply = args.get("apply").and_then(Value::as_bool).unwrap_or(false);
        let workspace_member = args
            .get("workspace_member")
            .and_then(Value::as_str)
            .map(String::from);

        let mut cmd: Vec<String> = vec!["fmt".into()];
        if let Some(m) = &workspace_member {
            cmd.push("-p".into());
            cmd.push(m.clone());
        }
        if !apply {
            cmd.push("--".into());
            cmd.push("--check".into());
        }
        let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        let out = run_cmd("cargo", &refs, &ctx.project_root, ctx.timeout_secs).await?;

        // rustfmt --check stampa "Diff in <path>:N:" per ogni file con diff.
        // Estraiamo i path unici per restituirli al chiamante.
        let files_changed = parse_rustfmt_files(&out.stdout);

        // `ok` significato:
        // - apply=true:  exit=0 → ok
        // - apply=false: exit=0 E nessun file → ok (codice già formattato)
        let ok = if apply {
            out.success()
        } else {
            out.success() && files_changed.is_empty()
        };

        Ok(json!({
            "ok": ok,
            "apply": apply,
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "files_changed": files_changed,
            "files_changed_count": files_changed.len(),
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "apply": {"type": "boolean", "description": "Se true applica le modifiche, altrimenti solo --check"},
                "workspace_member": {"type": "string"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        // `apply=true` può modificare sorgenti; default sicuro ma
        // dichiariamo write_subproc per sicurezza dispatcher.
        NexusToolSafety::write_subproc()
    }
}

/// Estrae dai log di `cargo fmt --check` i path unici segnalati come
/// "Diff in <path>:<line>:".
fn parse_rustfmt_files(stdout: &str) -> Vec<String> {
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Diff in ") {
            // Rest: "<path>:<line>:"
            if let Some(colon) = rest.find(':') {
                let path = &rest[..colon];
                if !path.is_empty() {
                    seen.insert(path.to_string());
                }
            }
        }
    }
    seen.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rustfmt_files_empty() {
        assert!(parse_rustfmt_files("").is_empty());
    }

    #[test]
    fn test_parse_rustfmt_files_single() {
        let out = "Diff in src/main.rs:10:\n  something\nDiff in src/main.rs:25:\n";
        let files = parse_rustfmt_files(out);
        assert_eq!(files, vec!["src/main.rs".to_string()]);
    }

    #[test]
    fn test_parse_rustfmt_files_multiple() {
        let out = "Diff in a.rs:1:\nDiff in b.rs:1:\nDiff in a.rs:2:\n";
        let files = parse_rustfmt_files(out);
        assert_eq!(files, vec!["a.rs".to_string(), "b.rs".to_string()]);
    }

    #[test]
    fn test_safety_is_write_subproc() {
        let s = FormatCodeTool.safety();
        assert!(s.can_execute_subproc);
        assert!(s.can_write_filesystem);
    }
}
