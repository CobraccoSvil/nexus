//! `security::sec_sql_injection_check` — rileva costruzione non sicura di query
//! SQL nel codice applicativo del progetto. Usa il detector unico
//! `mcp_quality::injection::detect_sql_injection` (ADR 0021): stessa logica dello
//! scanner del pannello Ottimizzazione, zero duplicazione. Non analizza i file
//! `.sql` (la injection e' un difetto del codice che costruisce la query).
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub struct SecSqlInjectionCheckTool;

/// Estensioni di codice analizzate dal detector.
const CODE_EXTS: &[&str] = &["rs", "py", "ts", "tsx", "js", "jsx", "mjs", "cjs"];

#[async_trait]
impl NexusToolHandler for SecSqlInjectionCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let mut files_scanned = 0usize;
        let mut high = 0usize;
        let mut medium = 0usize;
        let mut findings: Vec<Value> = Vec::new();

        let candidates: [PathBuf; 2] = [
            ctx.project_root.join("src"),
            ctx.project_root.join("crates"),
        ];
        for c in &candidates {
            if c.is_dir() {
                walk(
                    c,
                    &ctx.project_root,
                    &mut files_scanned,
                    &mut findings,
                    &mut high,
                    &mut medium,
                    0,
                );
            }
        }
        // Se non esistono src/ o crates/ (progetto con layout diverso), scansiona
        // direttamente la root del progetto.
        if !candidates.iter().any(|c| c.is_dir()) {
            walk(
                &ctx.project_root,
                &ctx.project_root,
                &mut files_scanned,
                &mut findings,
                &mut high,
                &mut medium,
                0,
            );
        }

        let interpolated_total = high + medium;
        Ok(json!({
            "ok": true,
            "files_scanned": files_scanned,
            "interpolated_total": interpolated_total,
            // Detector basato su pattern: non distingue piu' i singoli parametrizzati,
            // ma le righe sicure (sqlx::query!, .bind, execute con params, $1/?/:name)
            // sono escluse a monte e non contano come interpolazione.
            "parameterized_total": 0,
            "high": high,
            "medium": medium,
            "warning": interpolated_total > 0,
            "findings": findings,
        }))
    }
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}

#[allow(clippy::too_many_arguments)]
fn walk(
    dir: &Path,
    root: &Path,
    files: &mut usize,
    findings: &mut Vec<Value>,
    high: &mut usize,
    medium: &mut usize,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "target" || name.starts_with('.') || name == "node_modules" {
            continue;
        }
        if p.is_dir() {
            walk(&p, root, files, findings, high, medium, depth + 1);
            continue;
        }
        let is_code = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| CODE_EXTS.contains(&e))
            .unwrap_or(false);
        if !is_code {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&p) else {
            continue;
        };
        *files += 1;
        let rel = p
            .strip_prefix(root)
            .unwrap_or(&p)
            .to_string_lossy()
            .to_string();
        for inj in mcp_quality::injection::detect_sql_injection(&rel, &content) {
            if inj.severity == "high" {
                *high += 1;
            } else {
                *medium += 1;
            }
            findings.push(json!({
                "file": rel,
                "line": inj.line,
                "severity": inj.severity,
                "snippet": inj.snippet,
                "detail": inj.detail,
            }));
        }
    }
}
