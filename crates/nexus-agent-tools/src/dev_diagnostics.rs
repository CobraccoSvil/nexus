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
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use nexus_types::tool_outcome::{NaturaFallimento, RispostaTool};

use crate::input_contract::InputTool;
use crate::tool_inputs::NexusDevServerDiagnoseInput;

use super::ToolContextCore;

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

/// Diagnosi DB-driven riusabile (punto unico, regola L): ritorna il primo
/// pattern di `nexus_dev_diagnostics` che matcha il log, come
/// `(descrizione, fix_renderizzato, category)`. None se nessun pattern matcha
/// o la tabella e' vuota — in tal caso il chiamante usa il fallback hardcoded.
/// Usata sia dal tool sia da `services.rs` per non duplicare la diagnosi crash.
pub async fn diagnose_log_db(db: &PgPool, log: &str) -> Option<(String, String, String)> {
    let diagnostics = get_diagnostics(db).await;
    for diag in &diagnostics {
        if let Some(caps) = diag.pattern.captures(log) {
            let fix = render_fix_template(&diag.fix_template, &caps, None);
            let desc = if diag.description.is_empty() {
                "Errore rilevato nei log del servizio".to_string()
            } else {
                diag.description.clone()
            };
            return Some((desc, fix, diag.category.clone()));
        }
    }
    None
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
///
/// Ritorna l'errore di I/O INTERO invece di un messaggio gia' appiattito in
/// `String`: la natura del fallimento la legge `NaturaFallimento::da_errore_io`
/// dal `ErrorKind` (regola M — un file inesistente e un permesso negato mandano
/// l'agente su due strade diverse, e il testo del sistema operativo che li
/// distingue e' localizzato e cambia fra Windows e Linux). Appiattendo, quel
/// segnale strutturato spariva prima di poter essere letto.
async fn read_log_tail(path: &Path) -> Result<String, std::io::Error> {
    let meta = tokio::fs::metadata(path).await?;
    let size = meta.len() as usize;
    let start = size.saturating_sub(MAX_LOG_BYTES);
    if start == 0 {
        let bytes = tokio::fs::read(path).await?;
        return Ok(String::from_utf8_lossy(&bytes).to_string());
    }
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut f = tokio::fs::File::open(path).await?;
    f.seek(std::io::SeekFrom::Start(start as u64)).await?;
    let mut buf = Vec::with_capacity(MAX_LOG_BYTES);
    f.read_to_end(&mut buf).await?;
    Ok(String::from_utf8_lossy(&buf).to_string())
}

/// L'azione con cui l'agente rimedia quando la sorgente del log non e' leggibile.
/// Costante perche' i due rami che la nominano (percorso invalido, lettura
/// fallita) rimediano nello stesso modo, e due copie divergerebbero.
const HINT_SORGENTE: &str = "Passa log_path assoluto o relativo alla project root, \
                             oppure passa 'log' (stringa di log inline).";

/// Il log da diagnosticare e il FILE da cui proviene, quando proviene da un file.
///
/// I due nascono INSIEME perche' la provenienza e' parte del risultato: con
/// `log` e `log_path` entrambi valorizzati il contenuto veniva dall'inline, ma
/// il risultato dichiarava `log_path` come `log_source` e il placeholder
/// `{log_path}` dei fix nominava un file che nessuno aveva letto.
struct SorgenteLog {
    contenuto: String,
    /// `None` quando il log e' arrivato inline: non c'e' nessun file da nominare.
    file: Option<String>,
}

/// Risolve `log_path` in un percorso assoluto dentro la project root.
fn risolvi_path(ctx: &ToolContextCore, log_path: &str) -> Result<PathBuf, RispostaTool> {
    let p = Path::new(log_path);
    if p.is_absolute() {
        return Ok(p.to_path_buf());
    }
    // Il relativo passa dal punto unico (regola L), che de-duplica la root se
    // inclusa dall'agente e blocca "..".
    let errore = match nexus_types::workspace_paths::normalize_into_root(&ctx.root_path, log_path) {
        Ok(clean) => return Ok(ctx.root_path.join(&clean)),
        Err(e) => e,
    };
    // RIMEDIABILE: e' il parametro a essere sbagliato, e l'hint nomina le due
    // forme accettate piu' l'alternativa inline.
    let corpo = json!({
        "error": format!("log_path non valido: {}", errore.message()),
        "hint": HINT_SORGENTE,
    });
    Err(crate::errore_tool_con_dettagli(
        corpo,
        NaturaFallimento::Rimediabile,
    ))
}

/// Porta il log fino al diagnosticatore, da qualunque delle due sorgenti.
async fn carica_log(
    ctx: &ToolContextCore,
    params: &NexusDevServerDiagnoseInput,
) -> Result<SorgenteLog, RispostaTool> {
    // L'inline vince: e' il contenuto che l'agente ha gia' in mano.
    if let Some(inline) = params.log.clone() {
        return Ok(SorgenteLog {
            contenuto: inline,
            file: None,
        });
    }
    let log_path = params
        .log_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(lp) = log_path else {
        // RIMEDIABILE: mancano entrambi i parametri e il messaggio dice quali
        // sono e come ottenerli.
        let corpo = json!({
            "error": "Specifica almeno uno tra 'log_path' (file) o 'log' (stringa inline).",
            "hint": "Tipico uso: dopo run_service salva l'output in /tmp/<label>.log e passa \
                     log_path=/tmp/<label>.log, oppure passa in 'log' l'output di read_service_output.",
        });
        return Err(crate::errore_tool_con_dettagli(
            corpo,
            NaturaFallimento::Rimediabile,
        ));
    };
    let resolved = risolvi_path(ctx, lp)?;
    let errore = match read_log_tail(&resolved).await {
        Ok(contenuto) => {
            return Ok(SorgenteLog {
                contenuto,
                file: Some(lp.to_string()),
            })
        }
        Err(e) => e,
    };
    // La natura NON si sceglie a mano: viene dal `ErrorKind` (regola M).
    let corpo = json!({
        "error": format!("lettura log fallita su '{}': {errore}", resolved.display()),
        "hint": HINT_SORGENTE,
    });
    Err(crate::errore_tool_con_dettagli(
        corpo,
        NaturaFallimento::da_errore_io(&errore),
    ))
}

/// Confronta il log con i pattern attivi e compone i findings.
///
/// `file` e' la sorgente EFFETTIVAMENTE letta: alimenta il placeholder
/// `{log_path}` dei fix, che altrimenti nominerebbe un file mai aperto.
fn componi_findings(diagnostics: &[Diagnostic], log: &str, file: Option<&str>) -> Vec<Value> {
    let mut findings: Vec<Value> = Vec::new();
    // Dedup per (category + fix renderizzato): lo stesso fix piu' volte e'
    // rumore. Era una HashMap di contatori usata come insieme.
    let mut visti: std::collections::HashSet<String> = std::collections::HashSet::new();
    for diag in diagnostics {
        if findings.len() >= MAX_FINDINGS {
            break;
        }
        // Prendi il PRIMO match (i pattern sono ordinati per confidence DESC).
        let Some(caps) = diag.pattern.captures(log) else {
            continue;
        };
        let rendered_fix = render_fix_template(&diag.fix_template, &caps, file);
        if !visti.insert(format!("{}|{}", diag.category, rendered_fix)) {
            continue;
        }
        let matched_text = caps.get(0).map(|m| m.as_str()).unwrap_or_default();
        let finding = json!({
            "category": diag.category,
            "severity": diag.severity,
            "confidence": diag.confidence,
            "description": diag.description,
            "matched_excerpt": matched_text.chars().take(200).collect::<String>(),
            "fix_template_raw": diag.fix_template,
            "suggested_fix_action": parse_fix_action(&rendered_fix),
        });
        findings.push(finding);
    }
    findings
}

/// Il corpo del RAMO NUDO chiuso dalla migrazione: senza pattern attivi questo
/// tool non ha nulla con cui diagnosticare, e prima lo dichiarava come un
/// successo con `findings: []` — indistinguibile dal caso in cui i pattern ci
/// sono e nessuno matcha, che invece e' una diagnosi vera.
///
/// DEL SISTEMA e non `Rimediabile`: popolare `nexus_dev_diagnostics` e' un
/// INSERT dell'admin, e ripetere la chiamata rifallira' identica. Copre anche
/// il caso in cui la tabella non sia leggibile (vedi `load_diagnostics`): in
/// entrambi la strada per l'agente e' la stessa, leggere il log da solo.
fn corpo_senza_pattern() -> Value {
    json!({
        "error": "Nessun pattern attivo in nexus_dev_diagnostics: questo tool non ha con che cosa \
                  diagnosticare il log.",
        "hint": "Leggi il log direttamente (read_file, oppure read_service_output se il servizio e' \
                 in esecuzione). L'aggiunta di pattern e' un INSERT in nexus_dev_diagnostics, fuori \
                 dalla portata dell'agente.",
    })
}

/// Tool entry point.
pub async fn tool_nexus_dev_server_diagnose(
    ctx: &ToolContextCore,
    input: &Value,
) -> RispostaTool {
    let params = match NexusDevServerDiagnoseInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let sorgente = match carica_log(ctx, &params).await {
        Ok(s) => s,
        Err(risposta) => return risposta,
    };

    let diagnostics = get_diagnostics(&ctx.db).await;
    if diagnostics.is_empty() {
        return crate::errore_tool_con_dettagli(
            corpo_senza_pattern(),
            NaturaFallimento::DelSistema,
        );
    }

    let findings = componi_findings(&diagnostics, &sorgente.contenuto, sorgente.file.as_deref());
    // Un log che non matcha nessun pattern e' una diagnosi RIUSCITA con esito
    // negativo, non un fallimento: la ricerca e' andata a buon fine.
    let next_step_hint = if findings.is_empty() {
        "Nessun pattern noto matchato. Leggi l'output manualmente o aggiungi un pattern in \
         nexus_dev_diagnostics."
    } else {
        "Applica i fix in ordine di confidence (DESC). Dopo ogni fix, riavvia il servizio e \
         ri-chiama questo tool per verificare residui."
    };
    let corpo = json!({
        "log_source": sorgente.file.unwrap_or_else(|| "inline".to_string()),
        "port_hint": params.port,
        "findings_count": findings.len(),
        "findings": findings,
        "next_step_hint": next_step_hint,
    });
    RispostaTool::riuscito(corpo.to_string())
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
