//! `security::cargo_audit` — wrapper di `cargo audit --json`.
//!
//! Esegue `cargo audit` (che richiede `cargo install cargo-audit`) per
//! verificare vulnerabilità note (CVE) nelle dipendenze elencate in
//! `Cargo.lock`. Parsa il JSON output e ritorna una lista strutturata
//! di advisory con severity, package, version_affected.

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CargoAuditTool;

#[async_trait]
impl NexusToolHandler for CargoAuditTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let deny_warnings = args
            .get("deny_warnings")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let mut cmd: Vec<&str> = vec!["audit", "--json"];
        if deny_warnings {
            cmd.push("--deny");
            cmd.push("warnings");
        }
        let out = run_cmd("cargo", &cmd, &ctx.project_root, ctx.timeout_secs).await?;

        // cargo audit emette un singolo JSON blob su stdout, anche con
        // exit_code != 0 quando trova vulnerabilità.
        let parsed: Result<Value, _> = serde_json::from_str(&out.stdout);
        let advisory_summary = match parsed {
            Ok(v) => summarize_audit(&v),
            Err(_) => json!({
                "parse_error": true,
                "raw_stdout": out.stdout,
                "raw_stderr": out.stderr,
            }),
        };

        Ok(json!({
            "ok": out.success(),
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "audit": advisory_summary,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "deny_warnings": {"type": "boolean"}
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        // cargo audit scarica il database di advisory (rete) e legge
        // Cargo.lock — non scrive sorgenti ma write_subproc per via della
        // cache advisory-db.
        NexusToolSafety {
            read_only: false,
            can_write_filesystem: true,
            can_execute_subproc: true,
            network_egress: true,
        }
    }
}

/// Estrae i campi rilevanti dall'output JSON di cargo-audit.
///
/// Formato atteso (cargo-audit 0.17+):
/// ```json
/// {
///   "database": {...},
///   "lockfile": {...},
///   "vulnerabilities": {
///     "found": true,
///     "count": 2,
///     "list": [
///       {"advisory": {"id": "RUSTSEC-2023-0001", "package": "foo", "title": "..."}, ...}
///     ]
///   },
///   "warnings": {...}
/// }
/// ```
fn summarize_audit(v: &Value) -> Value {
    let vulns_found = v
        .get("vulnerabilities")
        .and_then(|x| x.get("found"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let vulns_count = v
        .get("vulnerabilities")
        .and_then(|x| x.get("count"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    let advisories: Vec<Value> = v
        .get("vulnerabilities")
        .and_then(|x| x.get("list"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            let adv = entry.get("advisory").cloned().unwrap_or(Value::Null);
            json!({
                "id": adv.get("id").cloned().unwrap_or(Value::Null),
                "package": adv.get("package").cloned().unwrap_or(Value::Null),
                "title": adv.get("title").cloned().unwrap_or(Value::Null),
                "severity": adv.get("severity").cloned().unwrap_or(Value::Null),
                "url": adv.get("url").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();

    json!({
        "vulnerabilities_found": vulns_found,
        "vulnerabilities_count": vulns_count,
        "advisories": advisories,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_summarize_empty() {
        let v = json!({"vulnerabilities": {"found": false, "count": 0, "list": []}});
        let s = summarize_audit(&v);
        assert_eq!(s["vulnerabilities_found"], false);
        assert_eq!(s["vulnerabilities_count"], 0);
    }

    #[test]
    fn test_summarize_one_advisory() {
        let v = json!({
            "vulnerabilities": {
                "found": true,
                "count": 1,
                "list": [{
                    "advisory": {
                        "id": "RUSTSEC-2023-0001",
                        "package": "openssl",
                        "title": "buffer overflow",
                        "severity": "high",
                        "url": "https://rustsec.org/..."
                    }
                }]
            }
        });
        let s = summarize_audit(&v);
        assert_eq!(s["vulnerabilities_count"], 1);
        assert_eq!(s["advisories"][0]["id"], "RUSTSEC-2023-0001");
        assert_eq!(s["advisories"][0]["severity"], "high");
    }
}
