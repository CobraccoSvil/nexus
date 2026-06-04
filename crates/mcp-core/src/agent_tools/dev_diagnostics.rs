//! Tool `nexus_dev_server_diagnose`: legge log/output di dev server (vite/next/
//! cargo/python) e ritorna findings strutturati con fix suggeriti basati su
//! pattern DB-driven (`nexus_dev_diagnostics`, mig 0232).
//!
//! Scopo: rendere "auto-healing" il loop iterativo del modello che, dopo aver
//! avviato `npm start` o `cargo run`, deve diagnosticare errori e applicare
//! fix. Senza questo tool il modello legge 200 righe di log, prova a capire,
//! spesso sbaglia o entra in loop. Con questo tool ottiene direttamente
//! `[{category, suggested_fix_action, confidence}]` da applicare uno per uno.
//!
//! Estensibile: nuovi pattern via INSERT in DB, niente deploy.

use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::AgentToolContext;

const CACHE_TTL: Duration = Duration::from_secs(60);
const MAX_FINDINGS: usize = 50;
const MAX_LOG_BYTES: usize = 200_000;

#[derive(Debug, Clone)]
struct Diagnostic {
    pattern: regex::Regex,
    category: String,
    fix_template: String,
    severity: String,
    confidence: i32,
    description: String,
}

struct DiagnosticsCache {
    entries: Vec<Diagnostic>,
    fetched_at: Instant,
}

static CACHE: Mutex<Option<DiagnosticsCache>> = Mutex::new(None);

async fn load_diagnostics(db: &PgPool) -> Vec<Diagnostic> {
    let rows = match sqlx::query(
        "SELECT pattern_regex, category, fix_template, severity, confidence, description \
         FROM nexus_dev_diagnostics WHERE enabled = true ORDER BY confidence DESC",
    )
    .fetch_all(db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("dev_diagnostics load fallita (tabella mancante?): {e}");
            return Vec::new();
        }
    };
    rows.iter()
        .filter_map(|r| {
            let pattern_str: String = r.try_get("pattern_regex").ok()?;
            let pattern = match regex::Regex::new(&pattern_str) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("dev_diagnostics: regex invalida '{pattern_str}': {e}");
                    return None;
                }
            };
            Some(Diagnostic {
                pattern,
                category: r.try_get("category").ok()?,
                fix_template: r.try_get("fix_template").ok()?,
                severity: r
                    .try_get::<String, _>("severity")
                    .unwrap_or_else(|_| "warning".into()),
                confidence: r.try_get::<i32, _>("confidence").unwrap_or(80),
                description: r.try_get::<String, _>("description").unwrap_or_default(),
            })
        })
        .collect()
}

async fn get_diagnostics(db: &PgPool) -> Vec<Diagnostic> {
    let needs_refresh = {
        let guard = CACHE.lock().unwrap();
        match guard.as_ref() {
            None => true,
            Some(c) => c.fetched_at.elapsed() > CACHE_TTL,
        }
    };
    if needs_refresh {
        let fresh = load_diagnostics(db).await;
        let mut guard = CACHE.lock().unwrap();
        *guard = Some(DiagnosticsCache {
            entries: fresh,
            fetched_at: Instant::now(),
        });
    }
    let guard = CACHE.lock().unwrap();
    guard
        .as_ref()
        .map(|c| c.entries.clone())
        .unwrap_or_default()
}

/// Sostituisce i placeholder {1},{2},... con i capture group della regex match.
/// Riconosce anche placeholder testuali: {file}, {from}, {module}, {log_path}.
fn render_fix_template(template: &str, caps: &regex::Captures, log_path: Option<&str>) -> String {
    let mut out = template.to_string();
    // Numerici {1},{2},...
    for i in 1..caps.len() {
        if let Some(g) = caps.get(i) {
            out = out.replace(&format!("{{{}}}", i), g.as_str());
        }
    }
    // Aliasi semantici dal primo capture (best effort)
    if let Some(first) = caps.get(1) {
        let v = first.as_str();
        out = out
            .replace("{file}", v)
            .replace("{from}", v)
            .replace("{module}", v);
    }
    if let Some(lp) = log_path {
        out = out.replace("{log_path}", lp);
    }
    out
}

/// Legge il file log troncando a MAX_LOG_BYTES (tail, gli errori utili sono recenti).
async fn read_log_tail(path: &Path) -> Result<String, String> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| format!("stat fallita su '{}': {e}", path.display()))?;
    let size = meta.len() as usize;
    let start = size.saturating_sub(MAX_LOG_BYTES);
    let bytes = if start == 0 {
        tokio::fs::read(path)
            .await
            .map_err(|e| format!("read fallita: {e}"))?
    } else {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let mut f = tokio::fs::File::open(path)
            .await
            .map_err(|e| e.to_string())?;
        f.seek(std::io::SeekFrom::Start(start as u64))
            .await
            .map_err(|e| e.to_string())?;
        let mut buf = Vec::with_capacity(MAX_LOG_BYTES);
        f.read_to_end(&mut buf).await.map_err(|e| e.to_string())?;
        buf
    };
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

/// Tool entry point.
pub(super) async fn tool_nexus_dev_server_diagnose(
    ctx: &AgentToolContext,
    input: &Value,
) -> String {
    // Input opzionali: log_path (file da scansionare), inline_log (stringa), port (per nota)
    let log_path = input
        .get("log_path")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string());
    let inline_log = input
        .get("log")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let port = input.get("port").and_then(Value::as_i64);

    let log_content = if let Some(s) = inline_log {
        s
    } else if let Some(lp) = log_path.as_ref() {
        // Risolvi path: se relativo, rispetto a project root
        let p = Path::new(lp);
        let resolved = if p.is_absolute() {
            p.to_path_buf()
        } else {
            ctx.root_path.join(lp)
        };
        match read_log_tail(&resolved).await {
            Ok(s) => s,
            Err(e) => {
                return json!({
                    "error": format!("lettura log fallita: {e}"),
                    "hint": "Passa log_path assoluto o relativo a project root, oppure passa 'log' (stringa inline)."
                })
                .to_string();
            }
        }
    } else {
        return json!({
            "error": "Specifica almeno uno tra 'log_path' (file) o 'log' (stringa inline).",
            "hint": "Tipico uso: dopo run_service salva l'output in /tmp/<label>.log e passa log_path=/tmp/<label>.log"
        })
        .to_string();
    };

    let diagnostics = get_diagnostics(&ctx.db).await;
    if diagnostics.is_empty() {
        return json!({
            "findings": [],
            "note": "Nessun pattern in nexus_dev_diagnostics. L'admin puo' aggiungere pattern via INSERT in DB."
        })
        .to_string();
    }

    let mut findings: Vec<Value> = Vec::new();
    let mut seen_categories: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for diag in &diagnostics {
        if findings.len() >= MAX_FINDINGS {
            break;
        }
        // Match: prendi il PRIMO match (i pattern sono ordinati per confidence DESC)
        if let Some(caps) = diag.pattern.captures(&log_content) {
            // Dedup per (category + fix renderizzato): stesso fix piu' volte e' rumore
            let rendered_fix = render_fix_template(&diag.fix_template, &caps, log_path.as_deref());
            let dedup_key = format!("{}|{}", diag.category, rendered_fix);
            let prev_count = seen_categories.get(&dedup_key).copied().unwrap_or(0);
            if prev_count >= 1 {
                continue;
            }
            seen_categories.insert(dedup_key.clone(), prev_count + 1);

            let matched_text = caps
                .get(0)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            findings.push(json!({
                "category": diag.category,
                "severity": diag.severity,
                "confidence": diag.confidence,
                "description": diag.description,
                "matched_excerpt": matched_text.chars().take(200).collect::<String>(),
                "fix_template_raw": diag.fix_template,
                "suggested_fix_action": parse_fix_action(&rendered_fix),
            }));
        }
    }

    json!({
        "log_source": log_path.unwrap_or_else(|| "inline".to_string()),
        "port_hint": port,
        "findings_count": findings.len(),
        "findings": findings,
        "next_step_hint": if findings.is_empty() {
            "Nessun pattern noto matchato. Leggi l'output manualmente o aggiungi un pattern in nexus_dev_diagnostics."
        } else {
            "Applica i fix in ordine di confidence (DESC). Dopo ogni fix, riavvia il servizio e ri-chiama questo tool per verificare residui."
        }
    })
    .to_string()
}

/// Parsa un fix_template renderizzato in un'azione strutturata che il modello
/// puo' eseguire direttamente. Formati supportati (vedi commento mig 0232).
fn parse_fix_action(rendered: &str) -> Value {
    if let Some(cmd) = rendered.strip_prefix("shell:") {
        return json!({
            "type": "run_command",
            "command": cmd,
        });
    }
    if let Some(pkg) = rendered.strip_prefix("install_pkg:") {
        return json!({
            "type": "run_command",
            "command": format!("npm install --save {}", pkg),
            "note": "Se il pkg e' devDep, usa --save-dev. Verifica con: npm view <pkg> peerDependencies",
        });
    }
    if let Some(rest) = rendered.strip_prefix("sed:") {
        // formato: glob:from:to
        let parts: Vec<&str> = rest.splitn(3, ':').collect();
        if parts.len() == 3 {
            return json!({
                "type": "run_command",
                "command": format!(
                    "grep -rln {:?} {} | xargs -r sed -i 's|{}|{}|g'",
                    parts[1], parts[0], parts[1], parts[2]
                ),
            });
        }
    }
    if let Some(rest) = rendered.strip_prefix("rewrite_import:") {
        let parts: Vec<&str> = rest.splitn(2, ':').collect();
        if parts.len() == 2 {
            return json!({
                "type": "run_command",
                "command": format!(
                    "grep -rln 'from \"{from}\"' src/ | xargs -r sed -i 's|from \"{from}\"|from \"{to}\"|g'",
                    from=parts[0], to=parts[1]
                ),
                "post_install_if_npm": format!("npm install {} ", parts[1]),
            });
        }
    }
    if let Some(rest) = rendered.strip_prefix("tool:") {
        let parts: Vec<&str> = rest.splitn(2, ':').collect();
        if parts.len() == 2 {
            let args: Value = serde_json::from_str(parts[1]).unwrap_or_else(|_| json!({}));
            return json!({
                "type": "invoke_tool",
                "tool_name": parts[0],
                "arguments": args,
                "hint": "Invocare via nexus_mcp_tool_call(server_id='builtin', tool_name=..., arguments=...)",
            });
        }
    }
    if let Some(rest) = rendered.strip_prefix("create_file:") {
        let parts: Vec<&str> = rest.splitn(2, ':').collect();
        if parts.len() == 2 {
            let body = match parts[1] {
                "vite_basic" => VITE_BASIC_INDEX_HTML,
                _ => "",
            };
            return json!({
                "type": "write_file",
                "path": parts[0],
                "content": body,
                "note": format!("Template id: {}", parts[1]),
            });
        }
    }
    // Fallback: ritorna come stringa raw
    json!({
        "type": "raw",
        "raw": rendered,
        "note": "Formato fix_template non riconosciuto. Interpreta manualmente.",
    })
}

const VITE_BASIC_INDEX_HTML: &str = r#"<!doctype html>
<html lang="it">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>App</title>
</head>
<body>
  <div id="root"></div>
  <script type="module" src="/src/main.tsx"></script>
</body>
</html>
"#;
