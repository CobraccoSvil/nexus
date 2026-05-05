//! `security::license_check` — verifica licenze delle dipendenze.
//!
//! Lancia `cargo metadata --format-version=1 --no-deps=false` e per ogni
//! package estrae il campo `license` dal manifest. Classifica in:
//! - `permissive` (MIT, Apache-2.0, BSD, ISC, Zlib, Unlicense)
//! - `copyleft` (GPL, AGPL, LGPL)
//! - `proprietary` (tutto il resto non SPDX-standard)
//! - `unknown` (campo license assente)
//!
//! Output:
//! ```json
//! {
//!   "total": 123,
//!   "by_category": {"permissive": 100, "copyleft": 2, "unknown": 21},
//!   "non_permissive": [{"name": "foo", "version": "1.0", "license": "GPL-3.0"}]
//! }
//! ```

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;

pub struct LicenseCheckTool;

#[async_trait]
impl NexusToolHandler for LicenseCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let out = run_cmd(
            "cargo",
            &["metadata", "--format-version=1"],
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

        let metadata: Value = serde_json::from_str(&out.stdout)?;
        let report = analyze_licenses(&metadata);

        Ok(json!({
            "total": report.total,
            "by_category": report.by_category,
            "non_permissive": report.non_permissive,
            "unknown": report.unknown,
            "duration_ms": out.duration_ms,
        }))
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only_subproc()
    }
}

#[derive(Default)]
struct LicenseReport {
    total: usize,
    by_category: HashMap<String, usize>,
    non_permissive: Vec<Value>,
    unknown: Vec<Value>,
}

fn categorize(license: &str) -> &'static str {
    if license.is_empty() {
        return "unknown";
    }
    let l = license.to_uppercase();
    // Copyleft forti
    if l.contains("GPL") && !l.contains("LGPL") {
        return "copyleft";
    }
    if l.contains("AGPL") {
        return "copyleft";
    }
    if l.contains("LGPL") {
        return "weak_copyleft";
    }
    // Permissive standard
    let permissive_markers = [
        "MIT",
        "APACHE",
        "BSD",
        "ISC",
        "ZLIB",
        "UNLICENSE",
        "CC0",
        "MPL",
        "BOOST",
    ];
    for m in permissive_markers {
        if l.contains(m) {
            return "permissive";
        }
    }
    "proprietary"
}

fn analyze_licenses(metadata: &Value) -> LicenseReport {
    let mut report = LicenseReport::default();
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for pkg in packages {
        report.total += 1;
        let name = pkg
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let version = pkg
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let license = pkg
            .get("license")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let category = categorize(&license);
        *report
            .by_category
            .entry(category.to_string())
            .or_insert(0) += 1;

        let entry = json!({
            "name": name,
            "version": version,
            "license": license,
            "category": category,
        });
        match category {
            "permissive" => {}
            "unknown" => report.unknown.push(entry),
            _ => report.non_permissive.push(entry),
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_categorize_permissive() {
        assert_eq!(categorize("MIT"), "permissive");
        assert_eq!(categorize("Apache-2.0"), "permissive");
        assert_eq!(categorize("MIT OR Apache-2.0"), "permissive");
        assert_eq!(categorize("BSD-3-Clause"), "permissive");
    }

    #[test]
    fn test_categorize_copyleft() {
        assert_eq!(categorize("GPL-3.0"), "copyleft");
        assert_eq!(categorize("AGPL-3.0"), "copyleft");
        assert_eq!(categorize("LGPL-2.1"), "weak_copyleft");
    }

    #[test]
    fn test_categorize_unknown() {
        assert_eq!(categorize(""), "unknown");
    }

    #[test]
    fn test_analyze_licenses_mixed() {
        let meta = json!({
            "packages": [
                {"name": "serde", "version": "1.0.0", "license": "MIT OR Apache-2.0"},
                {"name": "libgpl", "version": "2.0.0", "license": "GPL-3.0"},
                {"name": "nokey", "version": "0.1.0"},
            ]
        });
        let r = analyze_licenses(&meta);
        assert_eq!(r.total, 3);
        assert_eq!(r.by_category.get("permissive"), Some(&1));
        assert_eq!(r.by_category.get("copyleft"), Some(&1));
        assert_eq!(r.by_category.get("unknown"), Some(&1));
        assert_eq!(r.non_permissive.len(), 1);
        assert_eq!(r.unknown.len(), 1);
    }
}
