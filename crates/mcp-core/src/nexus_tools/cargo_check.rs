//! `code_analysis::cargo_check` — wrapper read-only di `cargo check`.
//!
//! Esegue `cargo check --message-format=json` sul progetto corrente e
//! parsa lo stream NDJSON di compiler message, estraendo errori e warning
//! in una struttura machine-readable.
//!
//! Input schema:
//! ```json
//! {
//!   "workspace_member": "string (optional, e.g. 'mcp-core')",
//!   "release": "bool (optional, default false)"
//! }
//! ```
//!
//! Output:
//! ```json
//! {
//!   "ok": true,
//!   "errors": [{"file": "...", "line": 42, "message": "..."}],
//!   "warnings": [{"file": "...", "line": 10, "message": "..."}],
//!   "duration_ms": 3421,
//!   "exit_code": 0
//! }
//! ```

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoCheckTool;

#[async_trait]
impl NexusToolHandler for CargoCheckTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        // Parse input
        let workspace_member = args
            .get("workspace_member")
            .and_then(Value::as_str)
            .map(|s| s.to_string());
        let release = args
            .get("release")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        // Costruisci args cargo
        let mut cargo_args: Vec<String> =
            vec!["check".to_string(), "--message-format=json".to_string()];
        if release {
            cargo_args.push("--release".to_string());
        }
        if let Some(member) = &workspace_member {
            cargo_args.push("-p".to_string());
            cargo_args.push(member.clone());
        }

        let cargo_args_ref: Vec<&str> = cargo_args.iter().map(|s| s.as_str()).collect();

        let out = run_cmd(
            "cargo",
            &cargo_args_ref,
            &ctx.project_root,
            ctx.timeout_secs,
        )
        .await?;

        // Punto unico in nexus_tools::parse_ndjson::extract_cargo_diagnostics (regola L, S42).
        let (errors, warnings) = super::parse_ndjson::extract_cargo_diagnostics(&out.stdout);

        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "errors": errors,
            "warnings": warnings,
            "workspace_member": workspace_member,
            "release": release,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "workspace_member": {
                    "type": "string",
                    "description": "Nome del package workspace da controllare (es. 'mcp-core'). Se assente, check l'intero workspace."
                },
                "release": {
                    "type": "boolean",
                    "description": "Se true, check in release mode. Default: false."
                }
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        // cargo check scrive in target/ → non è strictly read-only del
        // progetto, ma non modifica sorgenti. Usa preset write_subproc
        // con read_only=false per chiarezza: ogni chiamata può alterare
        // la cache di compilazione.
        NexusToolSafety::write_subproc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_input_schema_is_object() {
        let schema = CargoCheckTool.input_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["workspace_member"].is_object());
    }

    #[test]
    fn test_safety_flags() {
        let s = CargoCheckTool.safety();
        assert!(s.can_execute_subproc);
        assert!(s.can_write_filesystem);
    }

    // Test di parsing "offline": costruisce un NDJSON fittizio e verifica
    // che la logica di estrazione error/warning funzioni senza bisogno di
    // lanciare davvero cargo.
    #[test]
    fn test_ndjson_parse_logic_simulation() {
        let fake_stdout = r#"
{"reason":"compiler-message","message":{"level":"error","message":"cannot find type `Foo`","spans":[{"is_primary":true,"file_name":"src/lib.rs","line_start":10}]}}
{"reason":"compiler-message","message":{"level":"warning","message":"unused variable","spans":[{"is_primary":true,"file_name":"src/main.rs","line_start":5}]}}
{"reason":"build-finished","success":false}
"#;
        let mut errors = 0;
        let mut warnings = 0;
        for line in fake_stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let v: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v.get("reason").and_then(Value::as_str) != Some("compiler-message") {
                continue;
            }
            let level = v
                .get("message")
                .and_then(|m| m.get("level"))
                .and_then(Value::as_str)
                .unwrap_or("");
            match level {
                "error" => errors += 1,
                "warning" => warnings += 1,
                _ => {}
            }
        }
        assert_eq!(errors, 1);
        assert_eq!(warnings, 1);
    }
}
