//! Tool di operazioni su file: lettura, scrittura, lista, ricerca, edit, delete, rename.
//!
//! Estratto da mcp-core (split 7.4, passo agent_tools-5). Le funzioni del
//! monolite che chiudevano il ciclo di una mutazione (governance in scrittura,
//! tracking ripristinabile, autocommit di sessione, reindex/scan/lint
//! post-scrittura) restano li' e arrivano qui dal trait
//! [`crate::context_core::FileMutationHooks`].

use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio::process::Command;

use nexus_types::tool_outcome::NaturaFallimento;

use crate::context_core::ToolContextCore;
use crate::paths::resolve_relative_path;

// ── Helper spostati da mcp-core::agent_tools::helpers ──────────────────────
// Erano li' quando `files` viveva nello stesso package; nessun altro modulo li
// usa, quindi seguono il loro unico chiamante invece di restare a meta' strada.

/// Soglia oltre la quale `read_file` antepone una mappa strutturale del file
/// per orientare l'agente. NON tronca mai: il file viene comunque restituito
/// INTEGRALE (politica "mai troncare-e-buttare").
pub(crate) const READ_FILE_STRUCTURE_HINT_LINES: usize = 300;
/// Numero massimo di righe leggibili con read_file_lines in una singola chiamata.
/// read_file_lines e' un tool a RANGE esplicito (start/end), quindi non perde
/// dati: il chiamante itera i range. Valore molto alto per non spezzare
/// inutilmente letture ampie volute.
pub(crate) const READ_FILE_LINES_MAX: usize = 100_000;

/// File e pattern che l'agente non può mai modificare, indipendentemente dai permessi.
/// Proteggono secrets, configurazioni ambiente e il binario in produzione.
pub(crate) const PROTECTED_PATTERNS: &[&str] = &[
    ".env",
    ".env.local",
    ".env.production",
    ".env.staging",
    ".env.development",
    "nexus.env", // env specifico di Nexus
    "secrets",   // qualsiasi file con "secrets" nel nome
    "credentials",
    "id_rsa",
    "id_ed25519",
    ".pem",
    ".key",
    "Cargo.lock", // non modificare il lockfile manualmente
    "pnpm-lock.yaml",
];

/// Ritorna true se il path è protetto e non deve essere modificato dall'agente.
pub(crate) fn is_protected_path(path_str: &str) -> Option<&'static str> {
    let lower = path_str.to_lowercase();
    // Controlla nome file esatto o pattern nel path
    for pattern in PROTECTED_PATTERNS {
        let pat_lower = pattern.to_lowercase();
        // Match esatto del nome file o estensione
        if lower.ends_with(&pat_lower)
            || lower.contains(&format!("/{}", pat_lower))
            || lower.contains(&format!("\\{}", pat_lower))
            || lower == pat_lower
        {
            return Some(pattern);
        }
    }
    None
}

/// Estrae una mappa strutturale del file: funzioni, classi, componenti con numero di riga.
/// Supporta Rust, TypeScript/JavaScript, Python, C#, Go.
/// Usa corrispondenza su prefisso di parola chiave — nessuna regex, O(n) per riga.
pub(crate) fn extract_file_structure(content: &str) -> Vec<(usize, String)> {
    let mut entries: Vec<(usize, String)> = Vec::new();

    for (line_idx, raw_line) in content.lines().enumerate() {
        let line_num = line_idx + 1;
        let line = raw_line.trim();

        // Salta righe vuote e commenti
        if line.is_empty()
            || line.starts_with("//")
            || line.starts_with("/*")
            || line.starts_with('#')
        {
            continue;
        }

        // Helper: estrai nome identificatore dopo una keyword
        let ident_after = |s: &str, kw: &str| -> Option<String> {
            let rest = s.strip_prefix(kw)?.trim_start();
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name.is_empty() {
                None
            } else {
                Some(name)
            }
        };

        // Normalizza spazi multipli per matching keyword composte
        let normalized = line.split_whitespace().collect::<Vec<_>>().join(" ");

        // TypeScript/JavaScript — export function, async function, function
        if let Some(name) = [
            "export async function ",
            "export function ",
            "async function ",
            "function ",
        ]
        .iter()
        .find_map(|kw| ident_after(&normalized, kw))
        {
            entries.push((line_num, format!("fn {name}")));
            continue;
        }

        // TypeScript/JavaScript — export const X = (...) => / = async (
        if normalized.starts_with("export const ") || normalized.starts_with("const ") {
            // Solo se è assegnazione a funzione/arrow
            if normalized.contains("= (")
                || normalized.contains("= async (")
                || normalized.contains(": React.")
                || normalized.contains("FC =")
            {
                if let Some(name) = ident_after(&normalized, "export const ")
                    .or_else(|| ident_after(&normalized, "const "))
                {
                    entries.push((line_num, format!("const {name}")));
                    continue;
                }
            }
        }

        // class (TS/JS/Python/C#)
        if let Some(name) = ["export default class ", "export class ", "class "]
            .iter()
            .find_map(|kw| ident_after(&normalized, kw))
        {
            entries.push((line_num, format!("class {name}")));
            continue;
        }

        // Rust — pub async fn, pub fn, async fn, fn
        if let Some(name) = ["pub async fn ", "pub fn ", "async fn ", "fn "]
            .iter()
            .find_map(|kw| ident_after(&normalized, kw))
        {
            entries.push((line_num, format!("fn {name}")));
            continue;
        }

        // Rust — impl, struct, enum
        if let Some(name) = ident_after(&normalized, "impl ") {
            entries.push((line_num, format!("impl {name}")));
            continue;
        }
        if let Some(name) = ["pub struct ", "struct "]
            .iter()
            .find_map(|kw| ident_after(&normalized, kw))
        {
            entries.push((line_num, format!("struct {name}")));
            continue;
        }
        if let Some(name) = ["pub enum ", "enum "]
            .iter()
            .find_map(|kw| ident_after(&normalized, kw))
        {
            entries.push((line_num, format!("enum {name}")));
            continue;
        }

        // Python — def, async def
        if let Some(name) = ["async def ", "def "]
            .iter()
            .find_map(|kw| ident_after(&normalized, kw))
        {
            entries.push((line_num, format!("def {name}")));
            continue;
        }

        // C# — public/private/protected method or class
        if normalized.starts_with("public ")
            || normalized.starts_with("private ")
            || normalized.starts_with("protected ")
        {
            if normalized.contains(" class ")
                || normalized.contains(" interface ")
                || normalized.contains(" enum ")
            {
                let short: String = normalized.chars().take(60).collect();
                entries.push((line_num, format!("class {short}")));
                continue;
            }
            // method: ha parentesi aperta e non è una property semplice
            if normalized.contains('(') && !normalized.ends_with(';') {
                let short: String = normalized.chars().take(60).collect();
                entries.push((line_num, format!("method {short}")));
                continue;
            }
        }
    }

    entries
}

/// Risultato del preflight build graph (ADR 0020) applicato a write/edit.
/// Variant `Block(msg)` blocca la scrittura (es. file generato); `Warn(msg)`
/// lascia passare ma aggiunge un avviso testuale alla risposta del tool.
enum BuildGraphPreflight {
    Allow,
    Warn(String),
    Block(String),
}

/// Risolve il path di SCRITTURA fornito dall'LLM in un path assoluto confinato
/// alla `root`, de-duplicando la root se il modello l'ha gia' inclusa nel path.
///
/// Delega al PUNTO UNICO `nexus_types::workspace_paths::normalize_into_root`
/// (regola L), lo STESSO usato dalla lettura (`resolve_relative_path`): cosi'
/// lettura e scrittura risolvono i path in modo identico. Storicamente la
/// de-duplicazione viveva solo qui, percio' `read_file` falliva sui file che
/// `edit_file` scriveva quando l'LLM includeva la project_root nel path.
/// A differenza della lettura, questa NON richiede che il file esista
/// (i file nuovi non passerebbero `canonicalize`): normalizza e confina soltanto.
fn resolve_write_target(root: &std::path::Path, path_str: &str) -> Result<PathBuf, String> {
    let clean = nexus_types::workspace_paths::normalize_into_root(root, path_str)
        .map_err(|e| e.message().to_string())?;
    if clean.is_empty() {
        return Err("percorso vuoto".to_string());
    }
    Ok(root.join(&clean))
}

/// Esegue il preflight ADR 0020 su `path_str`. Ritorna `Allow` se il file e'
/// nel build graph o entry point o linguaggio non riconosciuto; `Warn` se
/// e' fuori dal build graph (warning non bloccante); `Block` se in directory
/// generata (es. node_modules, target, dist).
async fn run_build_graph_preflight(ctx: &ToolContextCore, path_str: &str) -> BuildGraphPreflight {
    // Estensioni codice rilevanti: l'enforcement parte solo per file
    // sorgente, non per md/json/yaml/config.
    let ext = std::path::Path::new(path_str)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    let is_code = matches!(
        ext.as_deref(),
        Some("ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "rs" | "py" | "go" | "rb")
    );
    if !is_code {
        return BuildGraphPreflight::Allow;
    }

    let rel = std::path::Path::new(path_str.trim_start_matches(['\\', '/']));
    let membership = match nexus_build_graph::is_in_build_graph(ctx.project_id, rel).await {
        Ok(m) => m,
        Err(e) => {
            // Cache non disponibile o resolver fallito: lascio passare ma loggo.
            tracing::debug!(
                project_id = %ctx.project_id,
                path = %path_str,
                error = %e,
                "build_graph.preflight: errore lookup, allow (best-effort)"
            );
            return BuildGraphPreflight::Allow;
        }
    };
    match membership {
        nexus_build_graph::BuildGraphMembership::Generated { reason } => {
            BuildGraphPreflight::Block(format!(
                "Scrittura rifiutata: '{}' e' un file generato ({}). I file generati dalla build non vanno modificati manualmente.",
                path_str, reason
            ))
        }
        nexus_build_graph::BuildGraphMembership::OutOfGraph { reason } => {
            let info_msg = build_graph_out_of_graph_info(ctx.project_id).await;
            BuildGraphPreflight::Warn(format!(
                "ATTENZIONE: '{}' NON e' nel build graph del progetto ({}). I file fuori dal build graph non vengono compilati ne eseguiti.{} Se il tuo obiettivo e' modificare codice di produzione, usa `nexus_build_graph_info` per verificare quale path e' nel build graph.",
                path_str, reason, info_msg
            ))
        }
        nexus_build_graph::BuildGraphMembership::Unknown { .. }
        | nexus_build_graph::BuildGraphMembership::InGraph { .. }
        | nexus_build_graph::BuildGraphMembership::Entrypoint { .. } => BuildGraphPreflight::Allow,
    }
}

/// Suffisso diagnostico per il warning "fuori dal build graph": sorgenti e
/// include-pattern da cui il grafo e' derivato. Stringa vuota se la cache non
/// e' disponibile o il calcolo fallisce (best-effort). Estratto da
/// `run_build_graph_preflight` per brevita'.
async fn build_graph_out_of_graph_info(project_id: uuid::Uuid) -> String {
    match nexus_build_graph::BuildGraphCache::global() {
        Some(cache) => match cache.get_or_compute(project_id).await {
            Ok(info) => format!(
                " Build graph derivato da: {}. Include patterns: {}.",
                info.sources.join(", "),
                info.include_globs.join(", ")
            ),
            Err(_) => String::new(),
        },
        None => String::new(),
    }
}

/// Legge un file. MIGRATO al contratto d'ingresso e a `RispostaTool`.
///
/// Ogni suo fallimento e' rimediabile dall'agente: un percorso sbagliato lo
/// corregge lui, e il messaggio dice quale.
pub async fn tool_read_file(
    ctx: &ToolContextCore,
    input: &Value,
) -> nexus_types::tool_outcome::RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::ReadFileInput};
    use nexus_types::tool_outcome::RispostaTool;

    let params = match ReadFileInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let path_str = params.path.as_str();
    let target = match resolve_relative_path(&ctx.root_path, path_str) {
        Ok(p) => p,
        Err(e) => {
            return RispostaTool::fallito_rimediabile(format!(
                "[Errore percorso: {}]",
                e.1["error"].as_str().unwrap_or("path error")
            ))
        }
    };
    // Cap-byte difensivo (governance fs/read_max_bytes): un file enorme
    // (bundle, dump, lock) caricato integralmente satura il contesto e la
    // memoria. Soglia DB-driven (regola G); 0/assente = nessun cap.
    if let Some(err) = read_max_bytes_guard(ctx, &target, path_str).await {
        // Il cap e' una soglia del progetto, ma l'agente PUO' rimediare
        // leggendo un intervallo con `read_file_lines`, e il messaggio glielo
        // dice: e' quello che rende la natura una promessa mantenuta.
        return RispostaTool::fallito_rimediabile(err);
    }

    let content = match tokio::fs::read_to_string(&target).await {
        Ok(c) => c,
        Err(e) => {
            return RispostaTool::fallito_rimediabile(format!("[Errore lettura '{path_str}': {e}]"))
        }
    };

    let total_lines = content.lines().count();
    if total_lines <= READ_FILE_STRUCTURE_HINT_LINES {
        // File piccolo: restituisci tutto
        return RispostaTool::riuscito(content);
    }

    let read_full_max_lines: usize =
        nexus_auth::get_setting_checked(&ctx.db, "agent.fs.read_full_max_lines")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(1200);
    RispostaTool::riuscito(render_large_file_response(
        &content,
        total_lines,
        path_str,
        read_full_max_lines,
    ))
}

/// Cap-byte difensivo sulla lettura integrale: se il file supera la soglia
/// `agent.fs.read_max_bytes` (DB-driven, regola G; 0/assente = nessun cap)
/// ritorna `Some(messaggio_errore)` che invita a `read_file_lines`. Estratto da
/// `tool_read_file`.
/// Cap-byte per le letture su file dei tool fs, da `agent.fs.read_max_bytes`
/// (DB-driven, regola G; 0/assente = nessun cap). PUNTO UNICO della lettura
/// (regola L): serve sia al guard di `read_file` sia alla ricerca in-process,
/// che senza cap leggerebbe in RAM file di qualunque dimensione.
async fn fs_read_max_bytes(db: &sqlx::PgPool) -> u64 {
    nexus_auth::get_setting_checked(db, "agent.fs.read_max_bytes")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(2_097_152)
}

async fn read_max_bytes_guard(
    ctx: &ToolContextCore,
    target: &Path,
    path_str: &str,
) -> Option<String> {
    let read_max_bytes: u64 = fs_read_max_bytes(&ctx.db).await;
    if read_max_bytes == 0 {
        return None;
    }
    let meta = tokio::fs::metadata(target).await.ok()?;
    if meta.len() > read_max_bytes {
        return Some(format!(
            "\u{274C} [Errore lettura '{}': file troppo grande ({} byte > limite {} byte). \
             Usa read_file_lines(path, start_line, end_line) per leggere una porzione, \
             o search_file_semantic per trovare le sezioni rilevanti.]",
            path_str,
            meta.len(),
            read_max_bytes
        ));
    }
    None
}

/// Compone la risposta per un file "grande" (oltre `READ_FILE_STRUCTURE_HINT_LINES`
/// righe): antepone una mappa strutturale per orientare l'agente. Per i file
/// medio-grandi restituisce il contenuto INTEGRALE subito dopo (politica "mai
/// troncare-e-buttare": nessuna riga persa). MA oltre `read_full_max_lines` il
/// contenuto integrale satura il contesto e, se l'agente rilegge identicamente
/// read_file (non avendo trovato subito la sezione), innesca un loop REALE:
/// incidente bookingService.ts 1711 righe -> 3 read_file identiche ->
/// loop_detected -> abort. Oltre la soglia si rimanda a read_file_lines guidati
/// dalla mappa (0/assente = sempre integrale). Estratto da `tool_read_file`.
/// Formatta la mappa strutturale (righe "  riga NNNN — descrizione") a partire
/// dalle definizioni estratte da `extract_file_structure`; placeholder se vuota.
/// Estratto da `render_large_file_response`.
fn format_structure_map(structure: &[(usize, String)]) -> String {
    if structure.is_empty() {
        return "  (nessuna struttura rilevata automaticamente)".to_string();
    }
    structure
        .iter()
        .map(|(ln, desc)| format!("  riga {:>4} — {}", ln, desc))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_large_file_response(
    content: &str,
    total_lines: usize,
    path_str: &str,
    read_full_max_lines: usize,
) -> String {
    let structure = extract_file_structure(content);
    let structure_map = format_structure_map(&structure);

    if read_full_max_lines > 0 && total_lines > read_full_max_lines {
        return format!(
            "[FILE GRANDE — {total_lines} righe — troppo lungo per la lettura integrale]\n\
            NON rileggere read_file su questo path: otterresti lo stesso output. Per procedere:\n\
            → leggi una sezione specifica con read_file_lines(\"{path_str}\", start_line, end_line) \
            usando le righe indicate dalla mappa qui sotto;\n\
            → oppure search_file_semantic(\"{path_str}\", \"cosa stai cercando\") per individuarla.\n\
            \n\
            === STRUTTURA DEL FILE ({struct_count} definizioni trovate) ===\n\
            {structure_map}",
            total_lines = total_lines,
            path_str = path_str,
            struct_count = structure.len(),
            structure_map = structure_map,
        );
    }

    format!(
        "[FILE GRANDE — {total_lines} righe totali — contenuto integrale incluso sotto]\n\
        → Per saltare a una sezione nota usa la mappa strutturale qui sotto.\n\
        → Per una ricerca mirata: search_file_semantic(\"{path_str}\", \"cosa stai cercando\").\n\
        \n\
        === STRUTTURA DEL FILE ({struct_count} definizioni trovate) ===\n\
        {structure_map}\n\
        \n\
        === CONTENUTO INTEGRALE ({total_lines} righe) ===\n\
        {content}",
        total_lines = total_lines,
        path_str = path_str,
        struct_count = structure.len(),
        structure_map = structure_map,
        content = content,
    )
}

/// Valida gli estremi `(start_line, end_line)` (1-based, inclusi) dichiarati da
/// `read_file_lines` e applica il cap `READ_FILE_LINES_MAX` sul range.
///
/// Prima di questa versione la funzione leggeva l'input GREZZO e accettava anche
/// `offset`/`limit` come alias. Quegli alias non sono mai stati nel catalogo, e
/// il prompt del supervisore glielo dice esplicitamente dalla migrazione 0060
/// («MAI usare "offset" o "limit" — quei parametri NON esistono in questo
/// tool»): il sistema prometteva al modello una cosa e l'handler ne accettava
/// un'altra, cioe' le due verita' che il contratto d'ingresso esiste per
/// unificare. Un input fuori schema lo ferma ora `errore_di_lettura`, che nomina
/// il campo sconosciuto — informazione sufficiente per correggere, che e' cio'
/// che [`NaturaFallimento::Rimediabile`] pretende.
///
/// Gli estremi arrivano come `i64` e non come `usize`: il modello puo' scrivere
/// un numero negativo, e un tipo che non lo rappresenta trasformerebbe un input
/// sbagliato in un errore di deserializzazione oscuro invece che in questo
/// controllo di dominio, che dice quale estremo e' fuori posto.
fn valida_intervallo(
    start_line: i64,
    end_line: i64,
) -> Result<(usize, usize), nexus_types::tool_outcome::RispostaTool> {
    use nexus_types::tool_outcome::RispostaTool;

    if start_line < 1 {
        return Err(RispostaTool::fallito_rimediabile(format!(
            "[Errore: 'start_line' deve essere un intero >= 1 (ricevuto {start_line})]"
        )));
    }
    if end_line < start_line {
        return Err(RispostaTool::fallito_rimediabile(format!(
            "[Errore: 'end_line' ({end_line}) deve essere >= 'start_line' ({start_line})]"
        )));
    }
    let start_line = start_line as usize;
    // Limita il range massimo per evitare di caricare troppe righe. La somma e'
    // saturante perche' gli estremi vengono dal modello: un `start_line` presso
    // il massimo rappresentabile non deve trasformare un input assurdo in un
    // panic da overflow: `render_line_range` lo respinge subito dopo, dicendo
    // quante righe ha il file.
    let end_line = (end_line as usize).min(start_line.saturating_add(READ_FILE_LINES_MAX - 1));
    Ok((start_line, end_line))
}

/// Legge un intervallo di righe. MIGRATO al contratto d'ingresso e a
/// `RispostaTool`.
pub async fn tool_read_file_lines(
    ctx: &ToolContextCore,
    input: &Value,
) -> nexus_types::tool_outcome::RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::ReadFileLinesInput};
    use nexus_types::tool_outcome::RispostaTool;

    let params = match ReadFileLinesInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let path_str = params.path.as_str();

    let (start_line, end_line) = match valida_intervallo(params.start_line, params.end_line) {
        Ok(range) => range,
        Err(risposta) => return risposta,
    };

    let target = match resolve_relative_path(&ctx.root_path, path_str) {
        Ok(p) => p,
        Err(e) => {
            return RispostaTool::fallito_rimediabile(format!(
                "[Errore percorso: {}]",
                e.1["error"].as_str().unwrap_or("path error")
            ))
        }
    };
    let content = match tokio::fs::read_to_string(&target).await {
        Ok(c) => c,
        // La natura viene dal `ErrorKind` (regola M): un file assente lo corregge
        // l'agente, un permesso negato no, e il messaggio del sistema operativo
        // non distingue i due in modo leggibile da codice.
        Err(e) => {
            return RispostaTool::fallito(format!("[Errore lettura '{path_str}': {e}]"))
                .con_natura(NaturaFallimento::da_errore_io(&e))
        }
    };

    render_line_range(&content, path_str, start_line, end_line)
}

/// Rende la porzione `[start_line, end_line]` (1-based, inclusi) del contenuto
/// con prefisso numerato "NNNN | testo" e un hint di continuazione se restano
/// righe. Fallimento esplicito se `start_line` supera il totale. Estratto da
/// `tool_read_file_lines`.
fn render_line_range(
    content: &str,
    path_str: &str,
    start_line: usize,
    end_line: usize,
) -> nexus_types::tool_outcome::RispostaTool {
    use nexus_types::tool_outcome::RispostaTool;

    let total_lines = content.lines().count();
    if start_line > total_lines {
        // Rimediabile, e il messaggio porta cio' che serve per rimediare: il
        // totale delle righe, da cui l'agente ricava un intervallo valido senza
        // dover rileggere il file.
        return RispostaTool::fallito_rimediabile(format!(
            "[Errore: start_line {start_line} supera il numero totale di righe del file \
             ({total_lines})]"
        ));
    }

    let end_line = end_line.min(total_lines);
    let selected: String = content
        .lines()
        .enumerate()
        .filter(|(i, _)| {
            let line_num = i + 1; // 1-based
            line_num >= start_line && line_num <= end_line
        })
        .map(|(i, line)| format!("{:>4} | {}", i + 1, line))
        .collect::<Vec<_>>()
        .join("\n");

    RispostaTool::riuscito(format!(
        "// {} — righe {}-{} (totale: {} righe)\n{}{}",
        path_str,
        start_line,
        end_line,
        total_lines,
        selected,
        if end_line < total_lines {
            format!("\n\n// ... righe {}-{} non mostrate. Usa read_file_lines(\"{}\", {}, {}) per continuare.",
                end_line + 1, total_lines, path_str, end_line + 1, (end_line + READ_FILE_LINES_MAX).min(total_lines))
        } else {
            String::new()
        }
    ))
}

/// Prepara la scrittura di `target`: crea le directory intermedie e registra il
/// tracking ripristinabile (mig 0349) leggendo lo stato PRIMA della scrittura,
/// cosi' un revert riporta il file allo stato attuale. Il record e' best-effort
/// (warn ma non blocca: l'agente non puo' restare bloccato per un bug della
/// tabella di audit). Estratto da `tool_write_file`.
///
/// Ritorna `(esisteva_gia, contenuto_invariato)`: il secondo e' `true` quando il
/// nuovo contenuto coincide byte per byte con quello gia' su disco, cioe' quando
/// la scrittura NON cambia nulla.
async fn prepare_write_and_track(
    ctx: &ToolContextCore,
    target: &Path,
    path_str: &str,
    content: &str,
) -> Result<(bool, bool), String> {
    // Crea directory intermedie se necessario
    if let Some(parent) = target.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return Err(format!("\u{274C} [Errore creazione directory: {}]", e));
        }
    }
    let existed_before = target.exists();
    let before_for_track: Option<String> = if existed_before {
        tokio::fs::read_to_string(target).await.ok()
    } else {
        None
    };
    // Il contenuto nuovo e' IDENTICO a quello gia' su disco? E' l'unico segnale
    // che distingue una scrittura che fa progredire il lavoro da una che gira a
    // vuoto. Va restituito all'agente: senza, un modello puo' riscrivere lo
    // stesso file decine di volte credendo di avanzare (incidente 2026-07-22:
    // 28 operazioni sullo stesso file, dimensione che oscillava avanti e
    // indietro, sub-run ucciso dal timeout senza mai convergere).
    let unchanged = before_for_track.as_deref() == Some(content);
    ctx.hooks
        .record_mutation(
            ctx,
            path_str,
            "write_file",
            before_for_track.as_deref(),
            Some(content),
        )
        .await;
    Ok((existed_before, unchanged))
}

/// Preambolo di `write_file`: permesso di scrittura, path presente e non
/// protetto, parametro `content` presente. Ritorna `(path_str, content)` o il
/// messaggio d'errore. Estratto da `tool_write_file`.
fn read_write_params(
    ctx: &ToolContextCore,
    input: &Value,
) -> Result<crate::tool_inputs::WriteFileInput, nexus_types::tool_outcome::RispostaTool> {
    use crate::{input_contract::InputTool, tool_inputs::WriteFileInput};
    use nexus_types::tool_outcome::RispostaTool;

    if !ctx.can_write {
        // Del sistema, come in `read_edit_params`: il permesso e' una decisione
        // del progetto e ritentare non lo cambia.
        return Err(RispostaTool::fallito_di_sistema(
            "[Errore: permesso di scrittura non concesso su questo progetto]",
        ));
    }
    let params = WriteFileInput::leggi(input)?;
    if !ctx.is_nexus_operator {
        if let Some(pattern) = is_protected_path(&params.path) {
            return Err(RispostaTool::fallito_rimediabile(format!(
                "[Errore: il file '{}' è protetto (pattern: '{}') e non può essere modificato dall'agente. Modifica manualmente se necessario.]",
                params.path, pattern
            )));
        }
    }
    Ok(params)
}

/// Scrive un file. MIGRATO al contratto d'ingresso e a `RispostaTool`.
pub async fn tool_write_file(
    ctx: &ToolContextCore,
    input: &Value,
) -> nexus_types::tool_outcome::RispostaTool {
    use nexus_types::tool_outcome::RispostaTool;

    let params = match read_write_params(ctx, input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let (path_str, content) = (params.path.as_str(), params.content.as_str());

    // Governance risorse in scrittura (porte ADR 0010 + URL interni), punto
    // unico con audit: su violazione registra in nexus_resource_audit e
    // ritorna il rifiuto. Catalogo policy: nexus_resource_policies (mig 0397).
    if let Some(msg) = ctx
        .hooks
        .enforce_on_write(ctx, "write_file", path_str, content)
        .await
    {
        // Il rifiuto dice QUALE risorsa e come chiederla: l'agente corregge.
        return RispostaTool::fallito_rimediabile(msg);
    }

    // Preflight build graph (ADR 0020): blocca file generati, avvisa OOG.
    let bg_warning = match run_build_graph_preflight(ctx, path_str).await {
        BuildGraphPreflight::Block(msg) => {
            return RispostaTool::fallito_rimediabile(format!("[Errore: {msg}]"))
        }
        BuildGraphPreflight::Warn(msg) => Some(msg),
        BuildGraphPreflight::Allow => None,
    };

    // Risoluzione path con de-duplicazione della root + confinamento (regola L).
    // Corregge il bug "<root>/<root>/file": un path che gia' contiene la root
    // (assoluto o relativo) viene normalizzato a relativo prima del join.
    let target = match resolve_write_target(&ctx.root_path, path_str) {
        Ok(p) => p,
        Err(e) => return RispostaTool::fallito_rimediabile(format!("[Errore: {e}]")),
    };

    let (existed_before, unchanged) =
        match prepare_write_and_track(ctx, &target, path_str, content).await {
            Ok(esito) => esito,
            Err(msg) => return RispostaTool::fallito_rimediabile(msg),
        };
    match tokio::fs::write(&target, content).await {
        Ok(()) => RispostaTool::riuscito(on_write_success(
            ctx,
            &target,
            path_str,
            content,
            existed_before,
            unchanged,
            bg_warning,
        )),
        Err(e) => {
            RispostaTool::fallito_rimediabile(format!("[Errore scrittura '{path_str}': {e}]"))
        }
    }
}

/// Post-scrittura riuscita di `write_file`: emette l'evento FileChanged
/// (created/modified), avvia i task di background (auto-commit + reindex/scan/
/// lint/doc) e compone il messaggio di successo. Estratto da `tool_write_file`.
fn on_write_success(
    ctx: &ToolContextCore,
    target: &Path,
    path_str: &str,
    content: &str,
    existed_before: bool,
    unchanged: bool,
    bg_warning: Option<String>,
) -> String {
    // Dispatcher: notifica Explorer/Editor in tempo reale
    nexus_events::dispatcher::emit(
        &ctx.project_channels,
        ctx.project_id,
        nexus_events::event::ProjectEvent::FileChanged {
            path: path_str.to_string(),
            op: if existed_before {
                "modified".to_string()
            } else {
                "created".to_string()
            },
        },
    );

    let autocommit_op = if existed_before { "modify" } else { "create" };
    spawn_autocommit_snapshot(ctx, autocommit_op, path_str);
    spawn_write_reindex(ctx, target, path_str, content);
    build_write_success_message(path_str, content.len(), unchanged, bg_warning)
}

/// Avvia in background lo snapshot di auto-commit per sessione su branch
/// dedicato (rete di sicurezza sopra `file_mutations`). Se non e' un git repo /
/// setting disabilitato / session_id assente, l'hook fa no-op silenzioso.
/// Punto unico (regola L) condiviso da `tool_write_file` e `tool_edit_file`.
fn spawn_autocommit_snapshot(ctx: &ToolContextCore, op: &str, path_str: &str) {
    ctx.hooks.spawn_autocommit_snapshot(ctx, op, path_str);
}

/// Avvia in background la re-indicizzazione del file nel code index, l'eventuale
/// auto-scan qualita', la ri-validazione delle violazioni di governance risorse
/// (regola H: niente residui nel pannello Problemi) e l'hook M2 di registrazione
/// documentazione (`upsert_project_document_if_doc`, abilitato dal `content`).
/// Estratto da `tool_write_file`.
fn spawn_write_reindex(ctx: &ToolContextCore, target: &Path, path_str: &str, content: &str) {
    ctx.hooks
        .spawn_post_write(ctx, target, path_str, Some(content));
}

/// Compone il messaggio di successo di `write_file`: riga base + eventuale
/// warning build-graph + nota B2 sulle config critiche (che richiedono il
/// riavvio dei servizi per avere effetto, mig 0438). Estratto da
/// `tool_write_file`.
fn build_write_success_message(
    path_str: &str,
    byte_len: usize,
    unchanged: bool,
    bg_warning: Option<String>,
) -> String {
    let mut msg = format!(
        "File '{}' scritto con successo ({} byte)",
        path_str, byte_len
    );
    // Segnale di NON-CONVERGENZA. La scrittura e' riuscita (quindi nessun
    // rilevatore di errori la nota) ma non ha cambiato NULLA: se il modello
    // ripete, sta girando a vuoto. Detto esplicitamente perche' l'esito
    // strutturato "successo" da solo e' indistinguibile da un progresso reale.
    if unchanged {
        msg.push_str(
            "\n\nATTENZIONE: il contenuto scritto e' IDENTICO a quello gia' \
             presente nel file: questa operazione NON ha modificato nulla. Se \
             stai cercando di correggere qualcosa, cambia approccio invece di \
             riscrivere lo stesso contenuto: leggi il file, individua la \
             differenza reale, oppure verifica se il problema e' altrove.",
        );
    }
    if let Some(w) = bg_warning {
        msg = format!("{}\n\n{}", msg, w);
    }
    // B2: se e' una config critica (.env, vite.config, package.json, ...),
    // SEGNALA (non prescrive, mig 0438) che i servizi gia' in ascolto non
    // applicheranno le modifiche finche' non vengono riavviati. Evita il
    // caso (incidente Beauty-Book) in cui l'agente cambia il .env del proxy
    // ma non riavvia il frontend, e la verifica gira sulla vecchia config.
    if is_critical_config(path_str) {
        msg = format!(
            "{}\n\nNota: questo e' un file di CONFIGURAZIONE. Un servizio gia' \
             in esecuzione non applichera' le modifiche finche' non viene \
             riavviato (es. Vite/Next leggono .env e config solo all'avvio). \
             Se un servizio del progetto e' attivo, riavvialo prima di \
             verificarne il comportamento.",
            msg
        );
    }
    msg
}

/// Vero se `path` e' un file di CONFIGURAZIONE critica le cui modifiche
/// richiedono il riavvio dei servizi gia' in ascolto per avere effetto (Vite,
/// Next, ecc. leggono questi file solo all'avvio; il .env governa proxy/porte).
/// Lista conservativa per evitare falsi positivi (B2). Funzione pura/testabile.
pub(crate) fn is_critical_config(path: &str) -> bool {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_lowercase();
    if name == ".env" || name.starts_with(".env.") {
        return true;
    }
    const EXACT: &[&str] = &["package.json", "cargo.toml", "dockerfile"];
    if EXACT.contains(&name.as_str()) {
        return true;
    }
    const PREFIXES: &[&str] = &[
        "vite.config.",
        "next.config.",
        "nuxt.config.",
        "astro.config.",
        "svelte.config.",
        "vue.config.",
        "tsconfig",
        "docker-compose",
    ];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Classifica un path `.md` (gia' lowercased) in uno dei `doc_type` ammessi dal
/// check constraint `project_documents_doc_type_check` (functional_analysis,
/// technical_analysis, er_diagram, project_management, release_notes). `None` se
/// il file non e' documentazione riconosciuta. Estratto da
/// `upsert_project_document_if_doc`.
fn classify_project_doc_type(lower: &str) -> Option<&'static str> {
    if lower.contains("prd")
        || lower.starts_with("specs/")
        || lower.contains("/specs/")
        || lower.contains("functional")
    {
        Some("functional_analysis")
    } else if lower.ends_with("readme.md")
        || lower.contains("architecture")
        || lower.starts_with("docs/")
        || lower.contains("/docs/")
        || lower.contains("technical")
    {
        Some("technical_analysis")
    } else if lower.contains("erd")
        || lower.contains("schema_diagram")
        || lower.contains("er_diagram")
    {
        Some("er_diagram")
    } else if lower.contains("changelog") || lower.contains("release_notes") {
        Some("release_notes")
    } else if lower.contains("contributing")
        || lower.contains("project_management")
        || lower.contains("roadmap")
    {
        Some("project_management")
    } else {
        None
    }
}

/// Titolo del documento: prima riga "# ..." del contenuto, oppure il nome file
/// senza estensione. Troncato a 255 caratteri. Estratto da
/// `upsert_project_document_if_doc`.
fn extract_doc_title(content: &str, rel_path: &str) -> String {
    let title = content
        .lines()
        .find_map(|l| {
            let t = l.trim();
            t.strip_prefix("# ").map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| {
            std::path::Path::new(rel_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(rel_path)
                .to_string()
        });
    title.chars().take(255).collect::<String>()
}

/// Hook M2: rileva se un file appena scritto e' documentazione del progetto e lo registra in `project_documents`.
/// Tipi rilevati: PRD, README, ARCHITECTURE, CHANGELOG, CONTRIBUTING, SPEC, generic markdown sotto specs/ o docs/.
/// Idempotente: se esiste gia una riga con stesso (project_id, file_path), aggiorna updated_at e version increment patch.
pub async fn upsert_project_document_if_doc(
    db: &sqlx::PgPool,
    project_id: uuid::Uuid,
    rel_path: &str,
    content: &str,
) -> Result<(), sqlx::Error> {
    let lower = rel_path.to_lowercase();
    if !lower.ends_with(".md") && !lower.ends_with(".markdown") {
        return Ok(());
    }
    let doc_type = match classify_project_doc_type(&lower) {
        Some(t) => t,
        None => return Ok(()),
    };
    let title = extract_doc_title(content, rel_path);

    sqlx::query(
        r#"
        INSERT INTO project_documents (project_id, doc_type, title, file_path, status, metadata, structure_json)
        VALUES ($1, $2, $3, $4, 'draft', jsonb_build_object('source', 'agent_write_file'), '{}'::jsonb)
        ON CONFLICT (project_id, file_path) DO UPDATE
          SET title = EXCLUDED.title,
              doc_type = EXCLUDED.doc_type,
              updated_at = NOW()
        "#,
    )
    .bind(project_id)
    .bind(doc_type)
    .bind(&title)
    .bind(rel_path)
    .execute(db)
    .await?;
    Ok(())
}

/// Le voci VISIBILI di una directory, ordinate, con `/` in coda alle
/// sottodirectory. Estratto da `tool_list_files`.
///
/// Le entry nascoste (nome che inizia con `.`) non compaiono: e' comportamento
/// storico di questo tool, ed e' la ragione per cui `file_exists` non puo'
/// dedurre da questo testo se un file esista — deve interrogare il filesystem.
/// Una entry illeggibile interrompe la raccolta senza far fallire il tool: cio'
/// che si e' potuto elencare vale piu' di un errore su tutto.
/// Variante RICORSIVA di [`raccogli_voci_visibili`], per `list_files` con
/// `recursive: true`. Ritorna percorsi relativi a `radice`, separatore `/`.
///
/// Le esclusioni delegano al PUNTO UNICO `nexus_tool_kit::is_skipped_dir`
/// (regola L), lo stesso di `classify_scan_entry`: copre i dotfile/dotdir —
/// comportamento storico della variante piatta — e gli alberi di build
/// (node_modules, target, dist, build). Senza, un elenco ricorsivo su una root
/// di sviluppo percorrerebbe decine di GB per rispondere a una domanda che
/// riguarda i sorgenti.
///
/// Il tetto sulle voci non e' cautela generica: il risultato finisce nel
/// contesto di un modello, e un elenco che lo satura fa perdere il turno a
/// prescindere da quanto sia completo. Raggiunto il tetto lo DICHIARA, invece
/// di troncare in silenzio facendo credere che l'albero finisca li'.
async fn raccogli_voci_ricorsive(radice: &Path) -> Vec<String> {
    /// Oltre questo numero di voci l'elenco si ferma e lo dichiara.
    const MAX_VOCI: usize = 2000;

    let mut out: Vec<String> = Vec::new();
    let mut da_visitare: Vec<(std::path::PathBuf, String)> =
        vec![(radice.to_path_buf(), String::new())];

    while let Some((dir, prefisso)) = da_visitare.pop() {
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            // Una directory illeggibile non fa fallire l'elenco: cio' che si e'
            // potuto raccogliere vale piu' di un errore su tutto (stessa scelta
            // della variante piatta).
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            if out.len() >= MAX_VOCI {
                out.push(format!(
                    "[elenco troncato a {MAX_VOCI} voci: usa 'directory' per restringere]"
                ));
                return out;
            }
            let nome = entry.file_name().to_string_lossy().to_string();
            if nexus_tool_kit::is_skipped_dir(&nome) {
                continue;
            }
            let relativo = if prefisso.is_empty() {
                nome.clone()
            } else {
                format!("{prefisso}/{nome}")
            };
            let is_dir = entry
                .file_type()
                .await
                .map(|t| t.is_dir())
                .unwrap_or(false);
            if is_dir {
                out.push(format!("{relativo}/"));
                da_visitare.push((entry.path(), relativo));
            } else {
                out.push(relativo);
            }
        }
    }
    out.sort();
    out
}

async fn raccogli_voci_visibili(entries: &mut tokio::fs::ReadDir) -> Vec<String> {
    let mut lines = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let kind = if entry.path().is_dir() { "/" } else { "" };
        lines.push(format!("{name}{kind}"));
    }
    lines.sort();
    lines
}

/// Elenca le voci di una directory. MIGRATO al contratto e a `RispostaTool`.
pub async fn tool_list_files(
    ctx: &ToolContextCore,
    input: &Value,
) -> nexus_types::tool_outcome::RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::ListFilesInput};
    use nexus_types::tool_outcome::RispostaTool;

    let params = match ListFilesInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    // `directory` e' opzionale nel catalogo, e la stringa vuota vale come la sua
    // assenza: entrambe significano "la root del progetto".
    let dir_str = params.directory.as_deref().unwrap_or("");
    let target = if dir_str.is_empty() {
        ctx.root_path.clone()
    } else {
        match resolve_relative_path(&ctx.root_path, dir_str) {
            Ok(p) => p,
            Err(e) => {
                return RispostaTool::fallito_rimediabile(format!(
                    "[Errore percorso: {}]",
                    e.1["error"].as_str().unwrap_or("path error")
                ))
            }
        }
    };

    let mut entries = match tokio::fs::read_dir(&target).await {
        Ok(rd) => rd,
        Err(e) => {
            return RispostaTool::fallito(format!("[Errore listing '{dir_str}': {e}]"))
                .con_natura(NaturaFallimento::da_errore_io(&e))
        }
    };

    let lines = if params.recursive.unwrap_or(false) {
        raccogli_voci_ricorsive(&target).await
    } else {
        raccogli_voci_visibili(&mut entries).await
    };
    if lines.is_empty() {
        // "vuota o non trovata" era una disgiunzione che il codice sa risolvere:
        // se `read_dir` e' RIUSCITA la directory esiste, punto — l'inesistenza
        // esce dal ramo `Err` qui sopra. Quel testo faceva credere il contrario
        // a chi lo leggeva, e un consumatore del final gate ci cercava dentro
        // "non trovato" per dedurre un fallimento che non c'era.
        RispostaTool::riuscito(format!(
            "Directory '{dir_str}' vuota (nessuna voce visibile)."
        ))
    } else {
        RispostaTool::riuscito(lines.join("\n"))
    }
}

/// Cerca un pattern nei file del progetto. MIGRATO al contratto e a
/// `RispostaTool`.
pub async fn tool_search_in_files(
    ctx: &ToolContextCore,
    input: &Value,
) -> nexus_types::tool_outcome::RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::SearchInFilesInput};
    use nexus_types::tool_outcome::RispostaTool;

    let params = match SearchInFilesInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let pattern = params.pattern.as_str();
    // Punto unico (regola L): de-duplica la root se l'agente l'ha inclusa nel
    // path e blocca il traversal ".." (resolve_relative_path -> normalize_into_root).
    let search_path: PathBuf = match params.path.as_deref() {
        Some(p) => match resolve_relative_path(&ctx.root_path, p) {
            Ok(path) => path,
            Err(e) => {
                return RispostaTool::fallito_rimediabile(format!(
                    "[Errore percorso: {}]",
                    e.1["error"].as_str().unwrap_or("path error")
                ))
            }
        },
        None => ctx.root_path.clone(),
    };

    let max_file_bytes = fs_read_max_bytes(&ctx.db).await;
    let stdout = match run_grep_or_fallback(pattern, &search_path, max_file_bytes).await {
        RisultatoRicerca::Grep(s) | RisultatoRicerca::RipiegoRust(s) => s,
        // Rimediabile, e la ragione e' strutturale: questo ramo scatta solo dove
        // `grep` esiste ed E' GIRATO emettendo un errore proprio (il fallback
        // Rust non fallisce mai — se la regex non compila degrada a ricerca
        // letterale). Cio' che grep rifiuta e' quasi sempre il pattern, che e'
        // l'unica cosa che ha scritto l'agente, e il suo messaggio viaggia qui
        // dentro: e' l'informazione con cui correggere.
        RisultatoRicerca::ErroreGrep(msg) => return RispostaTool::fallito_rimediabile(msg),
    };

    RispostaTool::riuscito(format_search_output(ctx, pattern, &stdout))
}

/// Chi ha risposto alla ricerca, e con quale esito (regola Q: l'esito sta in un
/// campo, non in una convenzione sulla stringa).
///
/// La distinzione fra `Grep` e `RipiegoRust` non e' decorativa: senza di essa
/// un test che chiede "la ricerca ha trovato il file?" resta verde anche dove
/// `grep` non e' installato e ha risposto lo scanner in-process — cioe' proprio
/// dove il difetto del path consegnato a un processo esterno non esiste. Un
/// verde per assenza (regola O) e' cio' che questo enum rende impossibile.
#[derive(Debug)]
pub(crate) enum RisultatoRicerca {
    /// `grep` e' girato e ha risposto: stdout nel formato "path:lineno:riga".
    Grep(String),
    /// `grep` non e' invocabile: ha risposto [`search_in_files_rust`].
    RipiegoRust(String),
    /// `grep` e' girato e ha emesso un errore proprio, da propagare.
    ErroreGrep(String),
}

/// Esegue `grep -rn --include=* --max-count=50 -I` su `search_path` e ne
/// ritorna lo stdout (formato "path:lineno:contenuto") dentro la variante che
/// dice CHI ha risposto. Se lo spawn fallisce (grep assente, tipico Windows
/// nativo) ripiega su [`search_in_files_rust`], che produce lo STESSO formato
/// cosi' il post-processing a valle resta unico (regola L).
/// [`RisultatoRicerca::ErroreGrep`] solo quando grep gira ed emette un errore
/// reale (stdout vuoto + stderr non vuoto), da propagare al chiamante.
///
/// Il fallback e' I/O SINCRONO su tutto l'albero, quindi gira in
/// `spawn_blocking`: su un worker tokio terrebbe fermo il thread per l'intera
/// scansione, e con esso i timer di ogni altro task servito da quel worker.
///
/// Le esclusioni (`is_skipped_dir`) valgono SOLO per il fallback: il ramo grep
/// non riceve `--exclude-dir` di proposito. `--exclude-dir` si applica anche
/// all'operando da riga di comando, quindi filtrare qui renderebbe muta una
/// ricerca esplicita dentro un albero escluso (`path='target'` -> zero righe).
/// Nel fallback il rischio non c'e': il filtro guarda i nomi delle entry
/// visitate, mai la root da cui si parte.
///
/// La resa del path per il processo esterno avviene QUI, non nel chiamante:
/// e' questa la funzione che lo consegna, e tenerla fuori significherebbe che
/// un test puo' attraversarla senza incontrare la resa — cioe' misurare una
/// strada che la produzione non percorre (regola O). Vale anche per il ripiego
/// in Rust, che non lancia processi ma produce le righe di output che
/// [`format_search_output`] deve poi riconoscere: se i due partissero da forme
/// diverse dello stesso path, il prefisso non verrebbe tolto e l'agente
/// riceverebbe percorsi assoluti al posto dei relativi.
async fn run_grep_or_fallback(
    pattern: &str,
    search_path: &Path,
    max_file_bytes: u64,
) -> RisultatoRicerca {
    let search_path =
        PathBuf::from(nexus_types::workspace_paths::path_per_processo_esterno(search_path));

    let output = Command::new("grep")
        .arg("-rn")
        .arg("--include=*")
        .arg("--max-count=50")
        .arg("-I") // ignora file binari
        .arg(pattern)
        .arg(&search_path)
        .output()
        .await;

    match output {
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            // grep esce con 1 e stdout vuoto quando non trova nulla: non e' un errore.
            if stdout.is_empty() && !stderr.is_empty() {
                return RisultatoRicerca::ErroreGrep(format!("[grep error: {}]", stderr.trim()));
            }
            RisultatoRicerca::Grep(stdout)
        }
        // grep assente (tipico Windows nativo): ricerca in-process, best-effort,
        // su un thread di blocking (mai su un worker del runtime).
        Err(_) => {
            let root = search_path;
            let pat = pattern.to_string();
            match tokio::task::spawn_blocking(move || {
                search_in_files_rust(&root, &pat, max_file_bytes)
            })
            .await
            {
                Ok(stdout) => RisultatoRicerca::RipiegoRust(stdout),
                // Panic nella scansione: esito STRUTTURATO al chiamante (regola
                // M), mai un unwrap che porterebbe giu' il tool runner.
                Err(e) => {
                    RisultatoRicerca::ErroreGrep(format!("[search error: scansione interrotta: {e}]"))
                }
            }
        }
    }
}

/// Formatta l'output della ricerca (comune a grep e al fallback Rust): rende i
/// path relativi alla root e applica il troncamento. Punto unico (regola L): la
/// stessa logica serviva sia al ramo grep sia al fallback Windows.
fn format_search_output(ctx: &ToolContextCore, pattern: &str, stdout: &str) -> String {
    formatta_risultati(&ctx.root_path, pattern, stdout)
}

/// La parte che decide davvero, senza il `ToolContextCore` attorno: di quel
/// contesto qui serve la sola root, e pretenderlo intero significherebbe che
/// per misurare la resa delle righe serve un DB. Separata per questo (regola
/// O), e `format_search_output` non fa altro che passarle la root.
fn formatta_risultati(root_path: &Path, pattern: &str, stdout: &str) -> String {
    // Limite massimo di output: 500KB. Risultati piu' grandi causano
    // RESOURCE_EXHAUSTED gRPC (limite 16MB client Python) e consumano
    // troppi token di contesto per l'LLM. 500KB ~ 10k righe di codice.
    const MAX_OUTPUT_BYTES: usize = 500 * 1024;
    const MAX_OUTPUT_LINES: usize = 2000;

    if stdout.is_empty() {
        return format!("Nessun risultato per '{pattern}'.");
    }
    // Rendi i path relativi alla root per leggibilita'. La root si confronta
    // nella STESSA resa con cui il path e' stato consegnato alla ricerca
    // (`run_grep_or_fallback`): e' quella la forma che torna in testa a ogni
    // riga, e due rese diverse dello stesso path non si riconoscerebbero — il
    // prefisso resterebbe li' e l'agente leggerebbe percorsi assoluti.
    let root_reso = nexus_types::workspace_paths::path_per_processo_esterno(root_path);
    let root_reso = root_reso.to_string_lossy();
    let lines: Vec<String> = stdout
        .lines()
        .map(|line| {
            line.replacen(root_reso.as_ref(), "", 1)
                .trim_start_matches(['/', '\\'])
                .to_string()
        })
        .collect();
    let total_lines = lines.len();
    // Troncamento: limita per numero righe e per dimensione bytes
    let mut result = String::new();
    for (count, line) in lines.iter().enumerate() {
        if count >= MAX_OUTPUT_LINES || result.len() + line.len() > MAX_OUTPUT_BYTES {
            let msg = format!(
                "\n\n[Risultato troncato: mostrate {} di {} righe. Usa un pattern piu' specifico o limita il path.]",
                count, total_lines
            );
            result.push_str(&msg);
            break;
        }
        if count > 0 {
            result.push('\n');
        }
        result.push_str(line);
    }
    result
}

/// Compila il predicato di match di riga per il fallback Rust: grep di default
/// interpreta il pattern come espressione regolare (BRE); `regex` usa ERE/PCRE-
/// like, che per i pattern comuni (letterali, classi, alternanze) coincide. Se
/// la compilazione fallisce si degrada a ricerca letterale (substring), sempre
/// case-sensitive. Estratto da `search_in_files_rust`.
fn compile_line_matcher(pattern: &str) -> impl Fn(&str) -> bool {
    let re = regex::Regex::new(pattern).ok();
    let literal = pattern.to_string();
    move |line: &str| match &re {
        Some(r) => r.is_match(line),
        None => line.contains(&literal),
    }
}

/// Ricerca ricorsiva in Rust puro, fallback cross-platform quando `grep` non e'
/// disponibile (Windows nativo). Riproduce il comportamento essenziale di
/// `grep -rn --max-count=50 -I`:
/// - cammina la directory con `std::fs` (nessuna dipendenza esterna);
/// - salta le entry nascoste (nome che inizia con '.') come il resto del modulo;
/// - salta i file binari (euristica: byte NUL nei primi 8 KB), come `-I`;
/// - al piu' 50 righe corrispondenti per file (`--max-count=50`);
/// - match case-sensitive come `grep` di default: `regex::Regex` (gia' dipendenza)
///   e, se il pattern non e' una regex valida, ricerca letterale con `contains`.
///
/// Formato riga identico a `grep -rn`: "<path_assoluto>:<lineno>:<contenuto>".
/// Cosa fare di una entry incontrata dalla scansione.
enum ScanEntry {
    /// Nome escluso, tipo illeggibile, o symlink/altro (non seguiti: cicli).
    Ignora,
    Directory(std::path::PathBuf),
    File(std::path::PathBuf),
}

/// Classifica una entry della DFS. Le esclusioni delegano al PUNTO UNICO
/// `nexus_tool_kit::is_skipped_dir` (regola L): copre i dotfile/dotdir
/// (comportamento storico di questo walk) E gli alberi di build - node_modules,
/// target, dist, build - che prima venivano percorsi per intero, su una root di
/// sviluppo decine di GB letti da un thread solo.
fn classify_scan_entry(entry: &std::fs::DirEntry) -> ScanEntry {
    if nexus_tool_kit::is_skipped_dir(&entry.file_name().to_string_lossy()) {
        return ScanEntry::Ignora;
    }
    match entry.file_type() {
        Ok(ft) if ft.is_dir() => ScanEntry::Directory(entry.path()),
        Ok(ft) if ft.is_file() => ScanEntry::File(entry.path()),
        _ => ScanEntry::Ignora,
    }
}

fn search_in_files_rust(root: &std::path::Path, pattern: &str, max_file_bytes: u64) -> String {
    let matches = compile_line_matcher(pattern);

    // Budget difensivo per non camminare all'infinito su alberi enormi: ben oltre
    // il troncamento a valle (2000 righe / 500 KB), quindi non altera il risultato.
    const MAX_FILES_VISITED: usize = 50_000;
    const MAX_TOTAL_MATCHES: usize = 5_000;

    let mut out = String::new();
    let mut total_matches = 0usize;
    let mut files_visited = 0usize;
    // DFS iterativa (niente ricorsione: alberi profondi non fanno overflow).
    let mut stack: Vec<std::path::PathBuf> = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => continue, // permessi/inesistente: best-effort, salta
        };
        for entry in rd.flatten() {
            let path = match classify_scan_entry(&entry) {
                ScanEntry::Ignora => continue,
                ScanEntry::Directory(p) => {
                    stack.push(p);
                    continue;
                }
                ScanEntry::File(p) => p,
            };
            files_visited += 1;
            if files_visited > MAX_FILES_VISITED || total_matches >= MAX_TOTAL_MATCHES {
                return out;
            }
            let remaining = MAX_TOTAL_MATCHES - total_matches;
            total_matches += append_file_matches(&path, &matches, remaining, max_file_bytes, &mut out);
        }
    }
    out
}

/// Cerca `matches` nel file `path` e appende in `out` le righe corrispondenti in
/// formato `grep -rn` ("path:lineno:contenuto"). Salta i binari (euristica `-I`:
/// byte NUL nei primi 8 KB) e i file illeggibili. Limita a min(50, `remaining`)
/// match. Ritorna il numero di match appesi. Estratto da `search_in_files_rust`.
fn append_file_matches(
    path: &Path,
    matches: &impl Fn(&str) -> bool,
    remaining: usize,
    max_file_bytes: u64,
    out: &mut String,
) -> usize {
    const MAX_MATCHES_PER_FILE: usize = 50;
    const BINARY_SNIFF_BYTES: usize = 8 * 1024;

    // Cap PRIMA della lettura, come fa `read_file` (read_max_bytes_guard): il
    // sniff binario guarda i primi 8 KB, ma senza questo controllo un .rlib o un
    // pack git da centinaia di MB sarebbe gia' stato letto INTERO in RAM per
    // scoprirlo. 0 = nessun cap (setting disattivato).
    if max_file_bytes > 0 {
        match std::fs::metadata(path) {
            Ok(meta) if meta.len() > max_file_bytes => return 0,
            Ok(_) => {}
            Err(_) => return 0,
        }
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return 0,
    };
    // Euristica `-I`: file binario se contiene un NUL nell'intestazione.
    if bytes.iter().take(BINARY_SNIFF_BYTES).any(|&b| b == 0) {
        return 0;
    }
    let cap = MAX_MATCHES_PER_FILE.min(remaining);
    let text = String::from_utf8_lossy(&bytes);
    let path_str = path.to_string_lossy();
    let mut per_file = 0usize;
    for (idx, line) in text.lines().enumerate() {
        if matches(line) {
            out.push_str(&format!("{}:{}:{}\n", path_str, idx + 1, line));
            per_file += 1;
            if per_file >= cap {
                break;
            }
        }
    }
    per_file
}

/// Elimina un file o una directory. MIGRATO al contratto e a `RispostaTool`.
pub async fn tool_delete_file(
    ctx: &ToolContextCore,
    input: &Value,
) -> nexus_types::tool_outcome::RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::DeleteFileInput};
    use nexus_types::tool_outcome::RispostaTool;

    if !ctx.can_write {
        // Del sistema, come in `write_file`: e' una decisione del progetto e
        // ritentare non la cambia.
        return RispostaTool::fallito_di_sistema(
            "[Errore: permesso di scrittura non concesso su questo progetto]",
        );
    }
    let params = match DeleteFileInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let path_str = params.path.as_str();
    let recursive = params.recursive.unwrap_or(false);

    let target = match resolve_relative_path(&ctx.root_path, path_str) {
        Ok(p) => p,
        // Un percorso fuori dalla root o inesistente: l'agente ne scrive un
        // altro, ed e' l'unica cosa che serve.
        Err(e) => {
            return RispostaTool::fallito_rimediabile(format!(
                "[Errore percorso: {}]",
                e.1["error"].as_str().unwrap_or("path error")
            ))
        }
    };

    if target.is_dir() {
        return delete_directory(&target, path_str, recursive).await;
    }
    match tokio::fs::remove_file(&target).await {
        Ok(()) => {
            nexus_events::dispatcher::emit(
                &ctx.project_channels,
                ctx.project_id,
                nexus_events::event::ProjectEvent::FileChanged {
                    path: path_str.to_string(),
                    op: "deleted".to_string(),
                },
            );
            RispostaTool::riuscito(format!("File '{}' eliminato con successo", path_str))
        }
        Err(e) => RispostaTool::fallito(format!("[Errore eliminazione '{}': {}]", path_str, e))
        .con_natura(NaturaFallimento::da_errore_io(&e)),
    }
}

/// Elimina una directory, ricorsivamente se `recursive`. Estratto da
/// `tool_delete_file`: il ramo non-ricorsivo suggerisce `recursive:true` se la
/// directory non e' vuota.
async fn delete_directory(
    target: &Path,
    path_str: &str,
    recursive: bool,
) -> nexus_types::tool_outcome::RispostaTool {
    use nexus_types::tool_outcome::RispostaTool;

    if recursive {
        match tokio::fs::remove_dir_all(target).await {
            Ok(()) => RispostaTool::riuscito(format!(
                "Directory '{}' eliminata ricorsivamente con successo",
                path_str
            )),
            Err(e) => RispostaTool::fallito(format!("[Errore eliminazione directory '{}': {}]", path_str, e))
            .con_natura(NaturaFallimento::da_errore_io(&e)),
        }
    } else {
        match tokio::fs::remove_dir(target).await {
            Ok(()) => {
                RispostaTool::riuscito(format!("Directory '{}' eliminata con successo", path_str))
            }
            // La direttiva sta gia' nel testo, ed e' l'unico caso in cui la
            // natura la sappiamo meglio del kind: `DirectoryNotEmpty` e'
            // rimediabile perche' esiste il flag, e il messaggio lo nomina.
            Err(e) => RispostaTool::fallito(
                format!(
                    "[Errore eliminazione directory '{}': {} (se non e' vuota usa recursive:true)]",
                    path_str, e
                ),
            )
            .con_natura(NaturaFallimento::da_errore_io(&e)),
        }
    }
}

/// Rinomina o sposta un file. MIGRATO al contratto e a `RispostaTool`.
pub async fn tool_rename_file(
    ctx: &ToolContextCore,
    input: &Value,
) -> nexus_types::tool_outcome::RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::RenameFileInput};
    use nexus_types::tool_outcome::RispostaTool;

    if !ctx.can_write {
        return RispostaTool::fallito_di_sistema(
            "[Errore: permesso di scrittura non concesso su questo progetto]",
        );
    }
    let params = match RenameFileInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let (from_str, to_str) = (params.from.as_str(), params.to.as_str());

    let from = match resolve_relative_path(&ctx.root_path, from_str) {
        Ok(p) => p,
        Err(e) => {
            return RispostaTool::fallito_rimediabile(format!(
                "[Errore percorso sorgente: {}]",
                e.1["error"].as_str().unwrap_or("path error")
            ))
        }
    };

    // Destinazione: file nuovo (non ancora esistente), quindi resolve_write_target
    // (non canonicalizza) e non resolve_relative_path. Punto unico (regola L):
    // de-duplica la root come per la sorgente e blocca traversal/uscita dalla root.
    let to = match resolve_write_target(&ctx.root_path, to_str) {
        Ok(p) => p,
        Err(e) => {
            return RispostaTool::fallito_rimediabile(format!(
                "[Errore percorso destinazione: {e}]"
            ))
        }
    };

    if let Some(parent) = to.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return RispostaTool::fallito(format!("[Errore creazione directory destinazione: {}]", e))
            .con_natura(NaturaFallimento::da_errore_io(&e));
        }
    }

    match tokio::fs::rename(&from, &to).await {
        Ok(()) => RispostaTool::riuscito(format!("Rinominato '{}' → '{}'", from_str, to_str)),
        Err(e) => RispostaTool::fallito(format!("[Errore rinomina '{}' → '{}': {}]", from_str, to_str, e))
        .con_natura(NaturaFallimento::da_errore_io(&e)),
    }
}

/// Estrae un prefisso "ancora" dalla prima riga di `old_string` da usare per
/// trovare la posizione approssimativa nel file. Tronca a 32 caratteri o al
/// primo separatore "strong" (`{`, `=`, `:`, `,`, `;`) — cosi' un old_string
/// stantio nel CORPO ma corretto nella TESTA della riga (es. firma di funzione
/// invariata, body cambiato) trova comunque l'ancora giusta nel file reale.
///
/// Esempi:
///   "pub fn target_function(arg: u32) -> u32 { arg + 2 }"
///     -> "pub fn target_function(arg" (taglio a 32 char)
///   "let foo = bar;"
///     -> "let foo " (taglio al primo `=`)
fn anchor_prefix(line: &str) -> &str {
    const MAX: usize = 32;
    const STOP_CHARS: &[char] = &['{', '=', ':', ',', ';'];
    let cut = line
        .char_indices()
        .find(|(_, c)| STOP_CHARS.contains(c))
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    let cut = cut.min(MAX).min(line.len());
    // Allinea al boundary char piu' vicino per evitare di tagliare un char
    // multibyte UTF-8 a meta'.
    let mut safe = cut;
    while safe > 0 && !line.is_char_boundary(safe) {
        safe -= 1;
    }
    line[..safe].trim_end()
}

/// Dove nel file si ADDENSANO di piu' le righe di `old_string`.
///
/// PERCHE' NON BASTA IL PRIMO TOKEN. L'ancora precedente prendeva la prima riga
/// non vuota di `old_string`, la troncava al primo `{`/`=`/`:`/`,`/`;`
/// ([`anchor_prefix`]) e restituiva la PRIMA riga del file che la contenesse.
/// Su codice con firme uniche (Rust, Python) funziona; su JSX/HTML/YAML e'
/// sistematicamente sbagliato, e per due ragioni che si sommano:
///
/// 1. il taglio al `=` butta via proprio cio' che distingue la riga
///    (`<div className="mt-4">` diventa `<div className`), tenendo la parte che
///    in quei file e' identica ovunque;
/// 2. `position()` prende la PRIMA occorrenza, non la migliore.
///
/// MISURATO il 07/08/2026 su biblioteca-scolastica: `App.jsx` ha 374 righe e
/// QUATTORDICI contengono `<div className`. L'agente cercava un blocco che
/// stava in fondo, l'ancora puntava alla riga 11, e l'errore gli mostrava le
/// righe 1..26 — il 7% del file, la parte sbagliata — con scritto «il contenuto
/// del file e' gia' incluso qui sotto». Ripeteva lo stesso `old_string` perche'
/// in quell'estratto non c'era nulla da cui correggersi: iterazioni 26 e 28
/// dello stesso run hanno `old_string` con la STESSA impronta md5. Su 144 edit
/// dei tre progetti vivi, 16 fallivano cosi' (11%).
///
/// IL CRITERIO. Si scorre il file con una finestra alta quanto `old_string` e
/// si conta quante sue righe (trimmate, non vuote) vi compaiono. Vince la
/// finestra col conteggio piu' alto. E' robusto per costruzione: non dipende da
/// separatori, quindi non ha un linguaggio preferito; tollera righe modificate,
/// perche' misura una sovrapposizione e non una corrispondenza esatta; e con
/// `old_string` di una sola riga degrada a «trova quella riga», che e' la
/// risposta giusta.
///
/// `None` quando NESSUNA riga di `old_string` compare nel file: li' non c'e'
/// una zona da mostrare, e il chiamante ripiega — il file e' probabilmente
/// diverso da quello che l'agente immaginava, ed e' un'informazione a sua volta.
fn ancora_per_sovrapposizione(lines: &[&str], old_string_lf: &str) -> Option<usize> {
    /// Tetto sulle righe di `old_string` considerate: oltre, il costo cresce
    /// senza migliorare l'ancora (le prime righe bastano a localizzare).
    const MAX_RIGHE_OLD: usize = 24;

    let old_righe: Vec<String> = old_string_lf
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .take(MAX_RIGHE_OLD)
        .map(|l| l.to_lowercase())
        .collect();
    if old_righe.is_empty() || lines.is_empty() {
        return None;
    }

    // Finestra alta quanto old_string (piu' un margine per le righe vuote che
    // abbiamo scartato), scorsa su tutto il file.
    let altezza = (old_righe.len() * 2).max(4).min(lines.len());
    let normalizzate: Vec<String> = lines.iter().map(|l| l.trim().to_lowercase()).collect();

    let mut migliore: Option<(usize, usize)> = None; // (punteggio, indice)
    for start in 0..normalizzate.len() {
        let end = (start + altezza).min(normalizzate.len());
        let finestra = &normalizzate[start..end];
        let punteggio = old_righe
            .iter()
            .filter(|o| finestra.iter().any(|f| f == *o))
            .count();
        if punteggio == 0 {
            continue;
        }
        // L'ancora e' la prima riga della finestra che appartiene DAVVERO a
        // `old_string`, non l'inizio della finestra: quella e' solo il punto da
        // cui abbiamo guardato, e puntarci sposta l'estratto di qualche riga
        // sopra il blocco cercato senza motivo.
        let Some(offset) = finestra.iter().position(|f| old_righe.contains(f)) else {
            continue;
        };
        // A parita' di punteggio vince la finestra piu' in ALTO (la prima che
        // lo raggiunge): senza questo, un file con blocchi ripetuti sceglierebbe
        // l'ultimo, che e' arbitrario quanto il primo ma meno prevedibile.
        if migliore.is_none_or(|(p, _)| punteggio > p) {
            migliore = Some((punteggio, start + offset));
        }
    }
    migliore.map(|(_, i)| i)
}

/// Ripiego: la riga che contiene il prefisso-ancora, scegliendo l'occorrenza
/// col contesto piu' promettente invece della prima.
///
/// Serve quando `old_string` non condivide NESSUNA riga intera col file — il
/// caso per cui [`anchor_prefix`] era nato: firma di funzione invariata, corpo
/// riscritto. Li' la sovrapposizione e' zero e il prefisso e' l'unico segnale.
fn ancora_da_prefisso(lines: &[&str], first_token: &str) -> Option<usize> {
    if first_token.is_empty() {
        return None;
    }
    let token = first_token.to_lowercase();
    lines
        .iter()
        .position(|l| l.to_lowercase().contains(&token))
}

/// Render NUMERATO di una finestra di righe `[start, end)` con cap per byte.
///
/// PUNTO UNICO (regola L) del rendering "estratto numerato del file reale":
/// usato sia dal ramo "old_string non trovato" sia dal ramo "old_string
/// ambiguo (N occorrenze)", cosi' l'agente vede SEMPRE lo stesso formato
/// `NNNN | testo` e puo' copiarne l'old_string esatto. Tronca in fondo se
/// supera `max_bytes` (la testa, di solito piu' utile, resta visibile).
/// Ritorna `(excerpt_senza_newline_finale, indice_ultima_riga_resa)`.
fn render_numbered_window(
    lines: &[&str],
    start: usize,
    end: usize,
    max_bytes: usize,
) -> (String, usize) {
    let mut excerpt = String::new();
    let mut bytes = 0usize;
    let mut last_rendered_idx = start;
    for (offset, line) in lines[start..end].iter().enumerate() {
        let line_number = start + offset + 1;
        let rendered = format!("{:>4} | {}\n", line_number, line);
        if bytes + rendered.len() > max_bytes {
            break;
        }
        bytes += rendered.len();
        excerpt.push_str(&rendered);
        last_rendered_idx = start + offset;
    }
    if excerpt.ends_with('\n') {
        excerpt.pop();
    }
    (excerpt, last_rendered_idx)
}

/// Indici (0-based) di riga in cui INIZIA ciascuna occorrenza di `needle` in
/// `content` (LF-normalizzato), limitate a `max_hits`. Una occorrenza che inizia
/// su una riga ma si estende su piu' righe e' contata una sola volta (alla riga
/// d'inizio). Usato dal ramo "old_string ambiguo" per mostrare il contesto delle
/// prime N occorrenze, cosi' l'agente sceglie quella univoca.
fn occurrence_start_lines(content: &str, needle: &str, max_hits: usize) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<usize> = Vec::new();
    let mut search_from = 0usize;
    while let Some(rel) = content[search_from..].find(needle) {
        let abs = search_from + rel;
        // Riga d'inizio = numero di '\n' prima dell'offset assoluto.
        let line_idx = content[..abs].bytes().filter(|b| *b == b'\n').count();
        hits.push(line_idx);
        if hits.len() >= max_hits {
            break;
        }
        // Avanza di almeno 1 byte per evitare loop su match a lunghezza zero
        // (gia' escluso da needle non vuoto, ma per overlap progressivo).
        search_from = abs + needle.len().max(1);
        if search_from >= content.len() {
            break;
        }
    }
    hits
}

/// Rende i blocchi "Occorrenza N (~riga M):" con l'estratto numerato attorno a
/// ciascuna hit (finestra +3/-6 righe, cap 900 byte per blocco). Estratto da
/// `build_old_string_ambiguous_message`; il valore ritornato e' gia' trimmato
/// dei newline finali. Riusa il punto unico [`render_numbered_window`].
fn render_occurrence_blocks(lines: &[&str], hit_lines: &[usize]) -> String {
    const WINDOW_BEFORE: usize = 3;
    const WINDOW_AFTER: usize = 6;
    const MAX_BYTES_PER_HIT: usize = 900;

    let total_lines = lines.len();
    let mut blocks = String::new();
    for (n, &hit) in hit_lines.iter().enumerate() {
        let start = hit.saturating_sub(WINDOW_BEFORE);
        let end = (hit + WINDOW_AFTER + 1).min(total_lines);
        let (excerpt, _) = render_numbered_window(lines, start, end, MAX_BYTES_PER_HIT);
        blocks.push_str(&format!(
            "Occorrenza {} (~riga {}):\n{}\n\n",
            n + 1,
            hit + 1,
            excerpt
        ));
    }
    // Rimuove i due newline finali per pulizia.
    blocks.trim_end().to_string()
}

/// Costruisce il messaggio di errore quando `edit_file` trova `old_string` PIU'
/// volte (deve essere univoco). Ramo reso actionable come il "non trovato":
/// mostra l'ESTRATTO NUMERATO attorno alle prime occorrenze, cosi' l'agente puo'
/// aggiungere righe di contesto e rendere l'old_string univoco SENZA chiamare
/// read_file (il contenuto e' gia' qui). Riusa il punto unico
/// [`render_numbered_window`].
fn build_old_string_ambiguous_message(
    content: &str,
    old_string_lf: &str,
    path_str: &str,
    count: usize,
) -> String {
    const MAX_HITS_SHOWN: usize = 3;

    let lines: Vec<&str> = content.lines().collect();
    let hit_lines = occurrence_start_lines(content, old_string_lf, MAX_HITS_SHOWN);

    // Fallback difensivo: se per qualche motivo non localizziamo le occorrenze
    // (es. old_string che attraversa confini in modo inatteso), restiamo sul
    // messaggio testuale storico — meglio che un estratto vuoto.
    if hit_lines.is_empty() {
        return format!(
            "\u{274C} [Errore: old_string trovato {} volte in '{}'. Deve essere unico: aggiungi piu' contesto (righe circostanti) per renderlo univoco.]",
            count, path_str
        );
    }

    let blocks = render_occurrence_blocks(&lines, &hit_lines);
    let more = if count > hit_lines.len() {
        format!(
            " (mostrate le prime {} di {} occorrenze)",
            hit_lines.len(),
            count
        )
    } else {
        String::new()
    };

    format!(
        "\u{274C} [Errore: old_string trovato {count} volte in '{path}' \u{2014} deve essere UNICO.{more}\n\
        \u{26a0} NON chiamare read_file: il contesto delle occorrenze e' gia' qui sotto.\n\
        Aggiungi al tuo old_string abbastanza righe circostanti (prese dall'estratto numerato) \
        da identificare UNA SOLA occorrenza, poi riprova:\n\n\
        {blocks}]",
        count = count,
        path = path_str,
        more = more,
        blocks = blocks,
    )
}

/// Costruisce il messaggio di errore quando `edit_file` non trova l'old_string.
///
/// Strategia anti-loop: oltre a indicare la riga approssimativa (token-match
/// case-insensitive), include un ESTRATTO NUMERATO del contenuto attuale del
/// file ATTORNO a quella riga (default +/- 15 righe, max 40 righe totali,
/// hard-cap ~2 KB) — cosi' l'agente puo' riformulare l'old_string esatto nello
/// stesso turno senza chiamare read_file (che potrebbe essere bloccato dal
/// loop-detector e che comunque sprecherebbe un tool-call).
///
/// Se il primo token di `old_string` non viene trovato, ripiega sulle prime
/// 40 righe del file (preview generica, comportamento storico ridotto).
///
/// Funzione pura per essere coperta da test unitari senza dipendenze runtime.
fn build_old_string_not_found_message(
    content: &str,
    old_string_lf: &str,
    path_str: &str,
) -> String {
    // Limiti dell'estratto (FIX hardening qualita' agentico):
    //  - WINDOW_BEFORE/AFTER controllano la finestra simmetrica attorno
    //    alla riga "simile"; valori conservativi per restare entro ~2 KB.
    //  - MAX_LINES e' un secondo hard-cap di sicurezza.
    //  - MAX_BYTES tronca per evitare di gonfiare il contesto su righe molto
    //    lunghe (minified, JSON serializzato, ecc.).
    const WINDOW_BEFORE: usize = 15;
    const WINDOW_AFTER: usize = 15;
    const MAX_LINES: usize = 40;
    const MAX_BYTES: usize = 2048;

    // Token-match: prima riga non vuota di old_string vs prima riga del file
    // che lo contiene (case-insensitive). E' un'ancora di navigazione, non
    // un match esatto: se manca anche questa, il file e' probabilmente
    // strutturalmente diverso da quello che l'agente immaginava.
    //
    // IMPORTANTE: troncare la prima riga ai primi ~32 char (o al primo
    // separatore strong: `{`, `=`, `(arg + `, ecc.) — altrimenti differenze
    // minime sul corpo (es. `arg + 1` vs `arg + 2` nell'old_string stantio)
    // farebbero fallire il match e ci ridurrebbero al fallback inizio-file,
    // perdendo proprio il valore di "estratto attorno alla riga giusta".
    let first_line = old_string_lf
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim())
        .unwrap_or("");
    let first_token = anchor_prefix(first_line);

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    // L'ancora viene da QUANTO SI SOMIGLIANO old_string e file, non dal primo
    // token: e' `ancora_per_sovrapposizione` a deciderlo. Il prefisso resta
    // solo come ultimo ripiego, e anche li' sceglie l'occorrenza MIGLIORE.
    let similar_line_idx: Option<usize> = ancora_per_sovrapposizione(&lines, old_string_lf)
        .or_else(|| ancora_da_prefisso(&lines, first_token));

    let approx_hint = if first_token.is_empty() {
        String::new()
    } else if let Some(i) = similar_line_idx {
        format!(" Prima riga simile trovata ~riga {}.", i + 1)
    } else {
        " Nessuna riga contiene il primo token di old_string.".to_string()
    };

    // Calcola finestra: se abbiamo una riga ancora usa +/- WINDOW_BEFORE/AFTER;
    // altrimenti fallback alle prime righe (caso "file totalmente diverso").
    let (start, end): (usize, usize) = match similar_line_idx {
        Some(i) => {
            let s = i.saturating_sub(WINDOW_BEFORE);
            let e = (i + WINDOW_AFTER + 1).min(total_lines);
            // Cap a MAX_LINES anche dopo l'espansione (in caso di window grande).
            let capped_end = (s + MAX_LINES).min(e);
            (s, capped_end)
        }
        None => (0, MAX_LINES.min(total_lines)),
    };

    // Render numerato + cap per byte (taglia in fondo se sfora MAX_BYTES, in
    // modo che la testa dell'estratto — di solito quella piu' utile — resti
    // sempre visibile). Punto unico del rendering: render_numbered_window.
    let (excerpt, last_rendered_idx) = render_numbered_window(&lines, start, end, MAX_BYTES);

    let lines_shown_end = last_rendered_idx + 1;
    let header_label = match similar_line_idx {
        Some(i) => format!(
            "Contenuto attuale attorno alla riga {} (righe {}..{})",
            i + 1,
            start + 1,
            lines_shown_end
        ),
        None => format!(
            "Contenuto attuale (righe {}..{})",
            start + 1,
            lines_shown_end
        ),
    };

    let more_hint = if lines_shown_end < total_lines {
        format!(
            "\n// ... {} righe non mostrate. Usa read_file_lines(\"{}\", {}, {}) se devi vedere altre sezioni.",
            total_lines - lines_shown_end,
            path_str,
            lines_shown_end + 1,
            (lines_shown_end + WINDOW_AFTER).min(total_lines)
        )
    } else {
        String::new()
    };

    format!(
        "\u{274C} [Errore: old_string non trovato nel file '{}'.{approx_hint}\n\
        \u{26a0} NON chiamare read_file o read_file_lines \u{2014} il contenuto del file e' gia' incluso qui sotto.\n\
        Confronta il tuo old_string con le righe reali e correggi spazi, newline o testo che differiscono:\n\n\
        {header_label}:\n{excerpt}{more_hint}]",
        path_str
    )
}

/// «Questa modifica risulta GIA' APPLICATA?»
///
/// Si pone SOLO quando `old_string` non e' stato trovato, ed e' l'altra
/// spiegazione possibile di quel fatto: o il testo da sostituire non e' mai
/// esistito, oppure e' gia' stato sostituito — da questa stessa chiamata
/// ripetuta, o da un'altra andata a buon fine.
///
/// MISURATO il 10/08/2026 sul run b8a9db1d (vetrina-statica): due `edit_file`
/// falliti su quattro, entrambi con `old_string` assente e `new_string` GIA'
/// presente nel file. Il messaggio diceva "correggi spazi, newline o testo che
/// differiscono" e mandava l'agente a cercare differenze di spaziatura in un
/// file di soli LF, dove non ce n'erano: una diagnosi che nomina una causa
/// inesistente non fa perdere solo un turno, ne fa perdere quanti ne servono
/// perche' il modello si arrenda.
///
/// `None` = non lo si puo' affermare. Il `new_string` VUOTO e' il caso da
/// escludere per primo: una cancellazione lascia un testo vuoto che
/// `contains` trova ovunque, e senza questa guardia OGNI old_string mancante
/// diventerebbe una "modifica gia' applicata".
fn modifica_gia_applicata(content: &str, new_string_lf: &str) -> Option<usize> {
    if new_string_lf.trim().is_empty() {
        return None;
    }
    match content.matches(new_string_lf).count() {
        0 => None,
        n => Some(n),
    }
}

/// Il testo dell'esito idempotente, composto DAI fatti (regola Q punto 3).
///
/// E' un SUCCESSO e non un fallimento perche' lo stato che l'agente voleva c'e'
/// gia': chiamarlo errore terrebbe vivo un giro che non ha piu' niente da
/// correggere. Ma dice anche, senza ambiguita', che QUESTA chiamata non ha
/// scritto nulla — «riuscito» e «ho cambiato qualcosa» non sono la stessa cosa,
/// e confonderli farebbe credere a un progresso che non c'e' stato.
///
/// Quando le occorrenze sono piu' d'una la si dichiara invece di tacerla: non
/// si puo' sapere quale sia opera di questa modifica, e un numero che non torna
/// e' un'informazione, non un dettaglio da nascondere.
fn messaggio_gia_applicata(path_str: &str, occorrenze: usize) -> String {
    let quante = if occorrenze == 1 {
        String::new()
    } else {
        format!(
            " Il testo nuovo compare {occorrenze} volte nel file: verifica che sia \
             cio' che intendevi prima di procedere."
        )
    };
    format!(
        "Nessuna modifica applicata a '{path_str}': il file e' gia' in questo stato. \
         L'old_string non e' presente e il new_string si', quindi la sostituzione \
         risulta gia' avvenuta — da questa stessa chiamata ripetuta o da una \
         precedente andata a buon fine.{quante} Non ripetere questo edit e non \
         rileggere il file per cercarne le differenze: non ce ne sono."
    )
}

#[cfg(test)]
mod modifica_idempotente {
    use super::{messaggio_gia_applicata, modifica_gia_applicata};

    /// IL CASO MISURATO il 10/08/2026 (run b8a9db1d su vetrina-statica): due
    /// `edit_file` su quattro falliti con `old_string` assente e `new_string`
    /// gia' presente. Il file era di soli LF, quindi la diagnosi «correggi
    /// spazi, newline o testo che differiscono» nominava una causa inesistente.
    ///
    /// MUTAZIONE: far ritornare `None` a `modifica_gia_applicata` quando il
    /// new_string c'e' -> si torna al messaggio di old_string non trovato, e
    /// questo test rosseggia.
    #[test]
    fn una_modifica_gia_applicata_non_e_un_old_string_sbagliato() {
        let file = "<div class=\"filter-bar\">
  <button>Tutti</button>
  <button>Azzera</button>
</div>
";
        let nuovo = "  <button>Azzera</button>";
        assert_eq!(modifica_gia_applicata(file, nuovo), Some(1));
        let msg = messaggio_gia_applicata("index.html", 1);
        assert!(msg.contains("gia' in questo stato"), "{msg}");
        assert!(
            !msg.contains("spazi") && !msg.contains("newline"),
            "non deve mandare a cercare differenze che non ci sono: {msg}"
        );
    }

    /// Una cancellazione ha `new_string` vuoto, e `contains("")` e' vero
    /// ovunque: senza la guardia OGNI old_string mancante diventerebbe una
    /// «modifica gia' applicata», cioe' il tool direbbe sempre di si'.
    ///
    /// MUTAZIONE: togliere la guardia sul vuoto -> questo test rosseggia con
    /// `Some(_)`.
    #[test]
    fn una_cancellazione_non_risulta_mai_gia_applicata() {
        assert_eq!(modifica_gia_applicata("qualunque contenuto", ""), None);
        assert_eq!(modifica_gia_applicata("qualunque contenuto", "   
  "), None);
    }

    /// Il testo nuovo assente e' il caso ordinario: l'old_string era davvero
    /// sbagliato, e il messaggio di prima resta quello giusto.
    #[test]
    fn se_il_testo_nuovo_non_c_e_resta_un_old_string_sbagliato() {
        assert_eq!(modifica_gia_applicata("alfa beta", "gamma"), None);
    }

    /// Piu' occorrenze non si tacciono: non si puo' sapere quale sia opera di
    /// questa modifica, e un numero che non torna e' un'informazione.
    #[test]
    fn le_occorrenze_multiple_si_dichiarano() {
        assert_eq!(modifica_gia_applicata("x
x
x", "x"), Some(3));
        let msg = messaggio_gia_applicata("f.txt", 3);
        assert!(msg.contains("3 volte"), "{msg}");
    }
}

/// Preambolo di `edit_file`: permesso di scrittura, path presente e non
/// protetto, parametri `old_string`/`new_string` presenti. Ritorna la tripla
/// `(path_str, old_string, new_string)` o il messaggio d'errore. Estratto da
/// `tool_edit_file`.
fn read_edit_params(
    ctx: &ToolContextCore,
    input: &Value,
) -> Result<crate::tool_inputs::EditFileInput, nexus_types::tool_outcome::RispostaTool> {
    use crate::{input_contract::InputTool, tool_inputs::EditFileInput};
    use nexus_types::tool_outcome::RispostaTool;

    if !ctx.can_write {
        // NON rimediabile dall'agente: il permesso e' una decisione del
        // progetto, e riprovare non lo cambia. E' il primo posto in cui la
        // distinzione fra le nature paga davvero.
        return Err(RispostaTool::fallito_di_sistema(
            "[Errore: permesso di scrittura non concesso su questo progetto]",
        ));
    }
    // I tre parametri arrivano dal CONTRATTO (`tool_input!`), cioe' dalla stessa
    // dichiarazione da cui nasce lo schema che il modello ha letto: non possono
    // divergere. Prima erano tre `input.get(...).ok_or_else(...)` con tre
    // messaggi scritti a mano — uno dei 52 tool che lo facevano.
    let params = EditFileInput::leggi(input)?;
    if !ctx.is_nexus_operator {
        if let Some(pattern) = is_protected_path(&params.path) {
            return Err(RispostaTool::fallito_rimediabile(format!(
                "[Errore: il file '{}' è protetto (pattern: '{}') e non può essere modificato dall'agente.]",
                params.path, pattern
            )));
        }
    }
    Ok(params)
}

/// L'editor chirurgico. MIGRATO alla regola Q: l'esito e la sua NATURA sono
/// campi, e il testo resta testo.
///
/// Ogni fallimento di questo tool e' [`NaturaFallimento::Rimediabile`], e non
/// per comodita': un `old_string` che non combacia, un blocco ambiguo, un
/// percorso protetto sono tutti errori che l'agente puo' correggere DA SOLO col
/// contenuto del messaggio — che infatti porta l'estratto numerato del file
/// reale. E' anche un impegno verificabile in senso stretto: se un ramo di
/// questo tool dichiarasse «rimediabile» senza dire come, la dichiarazione
/// sarebbe falsa.
pub async fn tool_edit_file(
    ctx: &ToolContextCore,
    input: &Value,
) -> nexus_types::tool_outcome::RispostaTool {
    use nexus_types::tool_outcome::RispostaTool;
    // La natura del fallimento la sceglie `read_edit_params`, che sa
    // distinguere un parametro sbagliato (rimediabile) da un permesso negato
    // dal progetto (del sistema): appiattirli qui rimetterebbe l'agente a
    // ritentare cio' che non puo' cambiare.
    let params = match read_edit_params(ctx, input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let (path_str, old_string, new_string) = (
        params.path.as_str(),
        params.old_string.as_str(),
        params.new_string.as_str(),
    );

    // Governance risorse in scrittura (porte + URL interni), punto unico con
    // audit: scansiona la nuova porzione e registra l'eventuale violazione.
    if let Some(msg) = ctx
        .hooks
        .enforce_on_write(ctx, "edit_file", path_str, new_string)
        .await
    {
        // Il rifiuto dice QUALE porta e come chiederne una: e' l'agente a
        // doverla correggere.
        return RispostaTool::fallito_rimediabile(msg);
    }

    // Preflight build graph (ADR 0020).
    let bg_warning = match run_build_graph_preflight(ctx, path_str).await {
        // Il marker in testa non serve piu': l'esito e' nel campo.
        BuildGraphPreflight::Block(msg) => {
            return RispostaTool::fallito_rimediabile(format!("[Errore: {msg}]"))
        }
        BuildGraphPreflight::Warn(msg) => Some(msg),
        BuildGraphPreflight::Allow => None,
    };

    let target = match resolve_relative_path(&ctx.root_path, path_str) {
        Ok(p) => p,
        Err(e) => {
            return RispostaTool::fallito_rimediabile(format!(
                "[Errore percorso: {}]",
                e.1["error"].as_str().unwrap_or("path error")
            ))
        }
    };

    edit_matched_content(
        ctx,
        &target,
        path_str,
        old_string,
        new_string,
        bg_warning,
        params.replace_all.unwrap_or(false),
    )
    .await
}

/// Legge il file, normalizza CRLF -> LF per un matching consistente e, in base al
/// numero di occorrenze di `old_string`, ritorna il messaggio di errore
/// (0 = non trovato, N>1 = ambiguo) o applica la sostituzione univoca. Estratto
/// da `tool_edit_file`.
/// I tre esiti dell'incontro fra `old_string` e il contenuto reale: nessuna
/// corrispondenza, piu' d'una, esattamente una.
///
/// I primi due sono fallimenti RIMEDIABILI, e il messaggio mantiene la
/// promessa: porta l'estratto numerato del file vero, ancorato alla zona dove
/// l'agente stava cercando. Il terzo applica la modifica.
async fn edit_matched_content(
    ctx: &ToolContextCore,
    target: &Path,
    path_str: &str,
    old_string: &str,
    new_string: &str,
    bg_warning: Option<String>,
    replace_all: bool,
) -> nexus_types::tool_outcome::RispostaTool {
    use nexus_types::tool_outcome::RispostaTool;
    let raw_content = match tokio::fs::read_to_string(target).await {
        Ok(c) => c,
        Err(e) => {
            return RispostaTool::fallito_rimediabile(format!("[Errore lettura '{path_str}': {e}]"))
        }
    };

    // Normalizza CRLF → LF per matching consistente (il code block nel prompt è sempre LF).
    // Se il file ha CRLF, old_string costruito dall'AI (LF) non matcherebbe altrimenti.
    // IMPORTANTE: ricordo se il file originale era CRLF, cosi' alla scrittura ripristino
    // gli EOL originali (altrimenti l'edit converte CRLF→LF tutto il file, generando
    // diff git enormi anche per modifiche minime — bug 14 del test E2E).
    let was_crlf = raw_content.contains("\r\n");
    let content = raw_content.replace("\r\n", "\n");
    let old_string_lf = old_string.replace("\r\n", "\n");
    let new_string_lf = new_string.replace("\r\n", "\n");

    let count = content.matches(old_string_lf.as_str()).count();
    match count {
        // Entrambi i messaggi portano l'estratto numerato del file reale: e'
        // cio' che rende la dichiarazione «rimediabile» una promessa mantenuta
        // e non un'etichetta.
        0 => match modifica_gia_applicata(&content, &new_string_lf) {
            Some(occorrenze) => RispostaTool::riuscito(messaggio_gia_applicata(
                path_str,
                occorrenze,
            )),
            None => RispostaTool::fallito_rimediabile(build_old_string_not_found_message(
                &content,
                &old_string_lf,
                path_str,
            )),
        },
        // Con `replace_all` la molteplicita' NON e' piu' un'ambiguita': e' cio'
        // che l'agente ha chiesto. Senza, resta il fallimento rimediabile di
        // sempre — e il messaggio spiega come rendere univoco l'old_string.
        n if n > 1 && !replace_all => RispostaTool::fallito_rimediabile(
            build_old_string_ambiguous_message(&content, &old_string_lf, path_str, n),
        ),
        n => RispostaTool::riuscito(
            apply_edit_and_persist(
                ctx,
                target,
                path_str,
                EditApply {
                    content_lf: &content,
                    old_string_lf: &old_string_lf,
                    new_string_lf: &new_string_lf,
                    raw_content: &raw_content,
                    was_crlf,
                    bg_warning,
                    sostituzioni: n,
                },
            )
            .await,
        ),
    }
}

/// Parametri della sostituzione da persistere in [`apply_edit_and_persist`].
/// Raggruppati per evitare una firma con troppi argomenti (clippy).
struct EditApply<'a> {
    /// Contenuto attuale LF-normalizzato del file.
    content_lf: &'a str,
    /// `old_string` LF-normalizzato (le occorrenze le ha gia' contate il
    /// chiamante, che dichiara quante sostituirne in [`Self::sostituzioni`]).
    old_string_lf: &'a str,
    /// Quante occorrenze sostituire: 1 nel caso univoco, N con `replace_all`.
    ///
    /// E' un NUMERO e non un booleano perche' il chiamante lo ha CONTATO: un
    /// `bool` costringerebbe questa funzione a ricontare per sapere quante ne
    /// ha toccate, cioe' a rifare la misura che ha gia' deciso il ramo.
    sostituzioni: usize,
    /// `new_string` LF-normalizzato che sostituisce l'occorrenza.
    new_string_lf: &'a str,
    /// Contenuto preesistente grezzo (EOL originali), per il tracking mutazioni.
    raw_content: &'a str,
    /// Vero se il file originale usava CRLF: gli EOL vengono ripristinati.
    was_crlf: bool,
    /// Warning build-graph da accodare al messaggio di successo.
    bg_warning: Option<String>,
}

/// Tracking ripristinabile (mig 0349) per un `edit_file`: registra before/after
/// PRIMA della scrittura (`before` e' il contenuto preesistente gia' letto dal
/// chiamante). Best-effort: warn ma non blocca. Estratto da
/// `apply_edit_and_persist`.
async fn record_edit_mutation(ctx: &ToolContextCore, path_str: &str, before: &str, after: &str) {
    ctx.hooks
        .record_mutation(ctx, path_str, "edit_file", Some(before), Some(after))
        .await;
}

/// Applica la sostituzione univoca (gia' validata dal chiamante), ripristina gli
/// EOL originali, registra il tracking mutazioni, scrive il file e avvia i task
/// di background (auto-commit + reindex/scan/lint). Ritorna il messaggio finale.
/// Estratto dal ramo di successo di `tool_edit_file`.
async fn apply_edit_and_persist(
    ctx: &ToolContextCore,
    target: &Path,
    path_str: &str,
    apply: EditApply<'_>,
) -> String {
    let new_content_lf =
        apply
            .content_lf
            .replacen(apply.old_string_lf, apply.new_string_lf, apply.sostituzioni);
    // Ripristina gli EOL originali del file (CRLF se l'originale era CRLF).
    // Senza questo, ogni edit di un file Windows convertirebbe l'intero file
    // in LF generando un diff git rumoroso (bug 14).
    let new_content = if apply.was_crlf {
        new_content_lf.replace('\n', "\r\n")
    } else {
        new_content_lf
    };
    record_edit_mutation(ctx, path_str, apply.raw_content, &new_content).await;
    match tokio::fs::write(target, &new_content).await {
        Ok(()) => {
            nexus_events::dispatcher::emit(
                &ctx.project_channels,
                ctx.project_id,
                nexus_events::event::ProjectEvent::FileChanged {
                    path: path_str.to_string(),
                    op: "modified".to_string(),
                },
            );
            // Auto-commit per sessione (vedi tool_write_file).
            spawn_autocommit_snapshot(ctx, "modify", path_str);
            spawn_edit_reindex(ctx, target, path_str);
            let mut base = format!(
                "File '{}' modificato con successo ({} byte → {} byte)",
                path_str,
                apply.content_lf.len(),
                new_content.len()
            );
            // Stesso segnale di non-convergenza di `write_file`: una sostituzione
            // puo' riuscire lasciando il file IDENTICO (old_string e new_string
            // equivalenti, o edit che annulla il precedente). L'esito
            // strutturato direbbe "successo" e il modello continuerebbe a
            // ritentare credendo di avanzare.
            if apply.raw_content == new_content {
                base.push_str(
                    "\n\nATTENZIONE: dopo la sostituzione il file e' IDENTICO a \
                     prima: questa modifica NON ha cambiato nulla. Non ripetere \
                     lo stesso edit; rileggi il file e verifica se il punto da \
                     correggere e' un altro.",
                );
            }
            match apply.bg_warning {
                Some(w) => format!("{}\n\n{}", base, w),
                None => base,
            }
        }
        Err(e) => format!("\u{274C} [Errore scrittura '{}': {}]", path_str, e),
    }
}

/// Avvia in background la re-indicizzazione del file dopo un `edit_file`.
/// Stesso hook di `spawn_write_reindex` con `content: None`: l'edit non ricrea
/// il .md da zero, quindi salta il solo hook M2 sui documenti. Le due funzioni
/// erano gemelle divergibili (stesse tre azioni ricopiate); ora convergono sul
/// punto unico `FileMutationHooks::spawn_post_write` (regola L).
fn spawn_edit_reindex(ctx: &ToolContextCore, target: &Path, path_str: &str) {
    ctx.hooks.spawn_post_write(ctx, target, path_str, None);
}

/// Crea una directory con semantica `-p` (idempotente, crea genitori).
/// Crea una directory. MIGRATO al contratto e a `RispostaTool`.
pub async fn tool_fs_mkdir(
    ctx: &ToolContextCore,
    input: &Value,
) -> nexus_types::tool_outcome::RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::FsMkdirInput};
    use nexus_types::tool_outcome::RispostaTool;

    if !ctx.can_write {
        return RispostaTool::fallito_di_sistema(
            "[Errore: permesso di scrittura non concesso su questo progetto]",
        );
    }
    let params = match FsMkdirInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let path_str = params.path.as_str();
    // `resolve_write_target` e non `resolve_relative_path`: il secondo
    // CANONICALIZZA, e canonicalizzare un percorso che non esiste ancora
    // fallisce con "Percorso non trovato" — cioe' esattamente il caso per cui
    // questo tool esiste. Con quel resolver `create_dir_all` era irraggiungibile
    // per qualunque directory nuova, e il tool riusciva SOLO su una directory
    // gia' presente, dove non aveva niente da fare. Stessa distinzione che
    // `rename_file` fa da sempre per la propria destinazione; il punto unico
    // de-duplica la root e blocca l'uscita dalla root in entrambi i casi.
    let target = match resolve_write_target(&ctx.root_path, path_str) {
        Ok(p) => p,
        Err(e) => return RispostaTool::fallito_rimediabile(format!("[Errore percorso: {e}]")),
    };
    if target.is_dir() {
        // Non e' un fallimento: cio' che l'agente voleva c'e' gia'.
        return RispostaTool::riuscito(format!("Directory '{}' esiste gia'", path_str));
    }
    match tokio::fs::create_dir_all(&target).await {
        Ok(()) => RispostaTool::riuscito(format!("Directory '{}' creata con successo", path_str)),
        Err(e) => RispostaTool::fallito(format!("[Errore creazione directory '{}': {}]", path_str, e))
        .con_natura(NaturaFallimento::da_errore_io(&e)),
    }
}

/// Copia un file o una directory (ricorsiva) dentro la root del progetto.
/// MIGRATO al contratto e a `RispostaTool`.
pub async fn tool_fs_copy(
    ctx: &ToolContextCore,
    input: &Value,
) -> nexus_types::tool_outcome::RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::FsCopyInput};
    use nexus_types::tool_outcome::RispostaTool;

    if !ctx.can_write {
        return RispostaTool::fallito_di_sistema(
            "[Errore: permesso di scrittura non concesso su questo progetto]",
        );
    }
    let params = match FsCopyInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let from_str = params.from.as_str();
    let to_str = params.to.as_str();
    let overwrite = params.overwrite.unwrap_or(false);

    let (from, to) = match resolve_from_to(&ctx.root_path, from_str, to_str) {
        Ok(pair) => pair,
        Err(risposta) => return risposta,
    };

    if !from.exists() {
        return RispostaTool::fallito_rimediabile(format!(
            "[Errore: sorgente '{from_str}' non esiste]"
        ));
    }

    if to.exists() && !overwrite {
        // Rimediabile nella forma piu' netta: il messaggio nomina il parametro
        // che risolve, e usarlo e' una decisione che spetta all'agente.
        return RispostaTool::fallito_rimediabile(format!(
            "[Errore: destinazione '{to_str}' esiste gia'. Usa overwrite:true per sovrascrivere]"
        ));
    }

    copy_from_to(&from, &to, from_str, to_str).await
}

/// Esegue la copia risolta: file singolo (creando le directory genitore) o
/// directory ricorsiva. Estratto da `tool_fs_copy` per coesione e brevita'.
async fn copy_from_to(
    from: &Path,
    to: &Path,
    from_str: &str,
    to_str: &str,
) -> nexus_types::tool_outcome::RispostaTool {
    use nexus_types::tool_outcome::RispostaTool;

    if from.is_file() {
        // Crea directory genitore se non esiste
        if let Some(parent) = to.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return RispostaTool::fallito(format!(
                    "[Errore creazione directory destinazione: {e}]"
                ))
                .con_natura(NaturaFallimento::da_errore_io(&e));
            }
        }
        match tokio::fs::copy(from, to).await {
            Ok(bytes) => RispostaTool::riuscito(format!(
                "File copiato '{from_str}' -> '{to_str}' ({bytes} byte)"
            )),
            Err(e) => RispostaTool::fallito(format!("[Errore copia file: {e}]"))
                .con_natura(NaturaFallimento::da_errore_io(&e)),
        }
    } else if from.is_dir() {
        match copy_dir_recursive(from, to).await {
            Ok(count) => RispostaTool::riuscito(format!(
                "Directory copiata '{from_str}' -> '{to_str}' ({count} file)"
            )),
            // La copia ricorsiva compone il proprio messaggio da piu' errori di
            // I/O possibili e non conserva un `ErrorKind` solo: la natura non si
            // indovina da quel testo (regola M), e resta quella che vale per
            // l'agente — riprovare la stessa copia non cambierebbe l'esito.
            Err(e) => RispostaTool::fallito_di_sistema(format!("[Errore copia directory: {e}]")),
        }
    } else {
        RispostaTool::fallito_rimediabile(format!(
            "[Errore: '{from_str}' non e' un file ne' una directory]"
        ))
    }
}

/// Helper ricorsivo per copia directory.
async fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<usize, String> {
    tokio::fs::create_dir_all(dst)
        .await
        .map_err(|e| format!("mkdir {}: {}", dst.display(), e))?;

    let mut entries = tokio::fs::read_dir(src)
        .await
        .map_err(|e| format!("readdir {}: {}", src.display(), e))?;

    let mut count = 0;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            count += Box::pin(copy_dir_recursive(&src_path, &dst_path)).await?;
        } else {
            tokio::fs::copy(&src_path, &dst_path)
                .await
                .map_err(|e| format!("copy {}: {}", src_path.display(), e))?;
            count += 1;
        }
    }
    Ok(count)
}

/// Risolve una coppia di path relativi (`from`/`to`) confinandoli alla root,
/// con messaggi d'errore distinti "sorgente"/"destinazione". Punto unico
/// (regola L) del pattern condiviso da `tool_fs_move`/`tool_fs_copy`.
fn resolve_from_to(
    root: &Path,
    from_str: &str,
    to_str: &str,
) -> Result<(PathBuf, PathBuf), nexus_types::tool_outcome::RispostaTool> {
    use nexus_types::tool_outcome::RispostaTool;

    let from = resolve_relative_path(root, from_str).map_err(|e| {
        RispostaTool::fallito_rimediabile(format!(
            "[Errore percorso sorgente: {}]",
            e.1["error"].as_str().unwrap_or("path error")
        ))
    })?;
    // La DESTINAZIONE non esiste ancora: e' il caso normale di una copia o di
    // uno spostamento. `resolve_relative_path` canonicalizza, quindi rifiutava
    // con "Percorso non trovato" ogni destinazione nuova — e i due tool
    // riuscivano SOLO verso un percorso gia' esistente, cioe' solo con
    // `overwrite:true`. Che il ramo fosse irraggiungibile lo diceva gia' il
    // codice: `tool_fs_copy` controlla `if to.exists() && !overwrite`, un test
    // che ha senso solo se `to` puo' non esistere.
    let to = resolve_write_target(root, to_str).map_err(|e| {
        RispostaTool::fallito_rimediabile(format!("[Errore percorso destinazione: {e}]"))
    })?;
    Ok((from, to))
}

/// Sposta (rinomina) un file o una directory. Atomico se sullo stesso
/// filesystem. MIGRATO al contratto e a `RispostaTool`.
pub async fn tool_fs_move(
    ctx: &ToolContextCore,
    input: &Value,
) -> nexus_types::tool_outcome::RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::FsMoveInput};
    use nexus_types::tool_outcome::RispostaTool;

    if !ctx.can_write {
        return RispostaTool::fallito_di_sistema(
            "[Errore: permesso di scrittura non concesso su questo progetto]",
        );
    }
    let params = match FsMoveInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let from_str = params.from.as_str();
    let to_str = params.to.as_str();

    let (from, to) = match resolve_from_to(&ctx.root_path, from_str, to_str) {
        Ok(pair) => pair,
        Err(risposta) => return risposta,
    };

    if !from.exists() {
        return RispostaTool::fallito_rimediabile(format!(
            "[Errore: sorgente '{from_str}' non esiste]"
        ));
    }
    if to.exists() {
        // A differenza di `fs_copy` qui non esiste un `overwrite`: la strada per
        // rimediare e' un'altra destinazione, o eliminare prima quella occupata.
        return RispostaTool::fallito_rimediabile(format!(
            "[Errore: destinazione '{to_str}' esiste gia']"
        ));
    }

    // Crea directory genitore se non esiste
    if let Some(parent) = to.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return RispostaTool::fallito(format!("[Errore creazione directory destinazione: {e}]"))
                .con_natura(NaturaFallimento::da_errore_io(&e));
        }
    }

    match tokio::fs::rename(&from, &to).await {
        Ok(()) => RispostaTool::riuscito(format!("Spostato '{from_str}' -> '{to_str}'")),
        Err(e) => RispostaTool::fallito(format!("[Errore spostamento: {e}]"))
            .con_natura(NaturaFallimento::da_errore_io(&e)),
    }
}

// Test unitari sulla funzione pura `build_old_string_not_found_message`.
// Verifica il branch di errore "old_string non trovato" cosi' che eventuali
// regressioni sull'estratto numerato attorno alla riga simile siano colte.
#[cfg(test)]
mod tests {
    use super::build_old_string_ambiguous_message;
    use super::build_old_string_not_found_message;
    use super::build_write_success_message;
    use super::is_critical_config;
    use super::occurrence_start_lines;

    use std::sync::{Arc, Mutex};

    use futures::future::BoxFuture;

    use crate::context_core::{FileMutationHooks, NoopEmbedder, ToolContextCore};

    // ── Il contratto degli hook di mutazione, visto dal tool ────────────────
    //
    // Le azioni attorno a una scrittura (gate di governance, tracking,
    // autocommit, reindex/scan/lint) vivono in mcp-core e arrivano qui da
    // `FileMutationHooks`. Da questo lato del confine l'unica cosa osservabile
    // e' SE il tool le invoca: un'implementazione che non chiamasse nulla
    // lascerebbe i tool verdi e la produzione senza governance. Questi test
    // guardano quello, attraversando i tool reali (`tool_write_file`,
    // `tool_edit_file`), non le funzioni interne.

    /// Hook che REGISTRA le chiamate invece di eseguirle.
    #[derive(Debug, Default)]
    struct HookRegistranti {
        eventi: Mutex<Vec<String>>,
        /// Se valorizzato, `enforce_on_write` RIFIUTA con questo messaggio.
        rifiuto: Option<String>,
    }

    impl HookRegistranti {
        fn eventi(&self) -> Vec<String> {
            self.eventi.lock().expect("lock eventi").clone()
        }

        fn annota(&self, e: String) {
            self.eventi.lock().expect("lock eventi").push(e);
        }
    }

    impl FileMutationHooks for HookRegistranti {
        fn reindex_file(
            &self,
            _: uuid::Uuid,
            _: std::path::PathBuf,
            _: std::path::PathBuf,
        ) -> BoxFuture<'static, ()> {
            Box::pin(async {})
        }

        fn enforce_on_write<'a>(
            &'a self,
            _: &'a ToolContextCore,
            tool_name: &'a str,
            path: &'a str,
            _: &'a str,
        ) -> BoxFuture<'a, Option<String>> {
            self.annota(format!("enforce:{tool_name}:{path}"));
            Box::pin(async move { self.rifiuto.clone() })
        }

        fn record_mutation<'a>(
            &'a self,
            _: &'a ToolContextCore,
            path: &'a str,
            tool_name: &'a str,
            _: Option<&'a str>,
            _: Option<&'a str>,
        ) -> BoxFuture<'a, ()> {
            self.annota(format!("record:{tool_name}:{path}"));
            Box::pin(async {})
        }

        fn spawn_autocommit_snapshot(&self, _: &ToolContextCore, op: &str, path: &str) {
            self.annota(format!("autocommit:{op}:{path}"));
        }

        fn spawn_post_write(
            &self,
            _: &ToolContextCore,
            _: &std::path::Path,
            path: &str,
            content: Option<&str>,
        ) {
            self.annota(format!("post_write:{path}:content={}", content.is_some()));
        }
    }

    /// Contesto reale (la struct di produzione) con gli hook sotto osservazione.
    /// Il pool e' lazy e non viene mai contattato: il ramo write/edit di un file
    /// non-codice non tocca il DB.
    fn ctx_di_prova(root: std::path::PathBuf, hooks: Arc<HookRegistranti>) -> ToolContextCore {
        let db = sqlx::PgPool::connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("pool lazy");
        ToolContextCore {
            root_path: root,
            user_id: uuid::Uuid::nil(),
            is_git_repo: false,
            can_write: true,
            project_id: uuid::Uuid::nil(),
            session_id: None,
            db: Arc::new(db.clone()),
            run_db: Arc::new(db),
            parent_run_id: None,
            run_id: None,
            long_running_patterns: Vec::new(),
            user_role: "admin".to_string(),
            is_nexus_operator: true,
            project_channels: Arc::new(dashmap::DashMap::new()),
            monitor_registry: Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
            hooks,
            embedder: Arc::new(NoopEmbedder),
            isolated_subrun: false,
            write_scope: Vec::new(),
        }
    }

    /// `write_file` deve attraversare TUTTI gli hook, e nell'ordine: il gate
    /// PRIMA di toccare il disco, il tracking prima della scrittura, autocommit
    /// e hook post-scrittura dopo. Mutazione che rende rosso: togliere una
    /// qualsiasi delle quattro chiamate da `tool_write_file`.
    #[tokio::test]
    async fn write_file_attraversa_tutti_gli_hook_di_mutazione() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hooks = Arc::new(HookRegistranti::default());
        let ctx = ctx_di_prova(dir.path().to_path_buf(), hooks.clone());

        let out = super::tool_write_file(
            &ctx,
            &serde_json::json!({ "path": "note.txt", "content": "ciao" }),
        )
        .await;

        // Il test guarda i CAMPI: e' il contratto che `write_file` ha adottato.
        assert_eq!(
            out.esito,
            nexus_types::tool_outcome::EsitoTool::Riuscito,
            "scrittura non riuscita: {}",
            out.testo
        );
        assert!(out.testo.contains("successo"), "testo: {}", out.testo);
        assert_eq!(
            hooks.eventi(),
            vec![
                "enforce:write_file:note.txt".to_string(),
                "record:write_file:note.txt".to_string(),
                "autocommit:create:note.txt".to_string(),
                // `content` valorizzato: e' il ramo che abilita l'hook M2 sui
                // documenti, l'unica differenza fra i due gemelli collassati.
                "post_write:note.txt:content=true".to_string(),
            ]
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("note.txt")).expect("file scritto"),
            "ciao"
        );
    }

    // ── tool_list_files: produttore REALE (non righe fabbricate a mano) ──────
    //
    // `check_file_exists`/`build_project_context` interpretavano il testo di
    // QUESTO tool con vocabolari propri (regola O): questi test lo eseguono
    // davvero, su un tempdir vero, e fissano cio' che produce davvero — non
    // cio' che un consumatore assumeva producesse.

    #[tokio::test]
    async fn list_files_salta_i_dotfile() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("README.md"), "x").expect("seed");
        std::fs::write(dir.path().join(".env"), "SECRET=1").expect("seed");
        let hooks = Arc::new(HookRegistranti::default());
        let ctx = ctx_di_prova(dir.path().to_path_buf(), hooks);

        let out = super::tool_list_files(&ctx, &serde_json::json!({})).await;

        assert!(out.testo.contains("README.md"), "listing: {}", out.testo);
        // Il dotfile NON compare nel listing per un umano: e' proprio la ragione
        // per cui `file_exists` non puo' piu' fidarsi di questo testo per
        // decidere se un file esiste (deve interrogare il filesystem).
        assert!(!out.testo.contains(".env"), "listing: {}", out.testo);
    }

    #[tokio::test]
    async fn list_files_directory_vuota_non_e_un_fallimento() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hooks = Arc::new(HookRegistranti::default());
        let ctx = ctx_di_prova(dir.path().to_path_buf(), hooks);

        let out = super::tool_list_files(&ctx, &serde_json::json!({})).await;

        // Una directory VUOTA E' un successo (list_dir e' riuscita, semplicemente
        // non ha trovato voci): il vecchio testo "vuota o non trovata" affermava
        // anche il contrario, e un consumatore ci cercava "non trovat*" dentro
        // per dedurre un fallimento che non c'era mai stato.
        assert_eq!(
            out.esito,
            nexus_types::tool_outcome::EsitoTool::Riuscito,
            "{}",
            out.testo
        );
        assert!(!out.testo.to_lowercase().contains("non trovata"), "{}", out.testo);
    }

    #[tokio::test]
    async fn list_files_directory_assente_e_un_fallimento_dichiarato() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hooks = Arc::new(HookRegistranti::default());
        let ctx = ctx_di_prova(dir.path().to_path_buf(), hooks);

        let out = super::tool_list_files(&ctx, &serde_json::json!({ "directory": "assente" }))
            .await;

        // QUESTO e' un fallimento vero, e ora vive nel CAMPO invece che in un
        // marker in testa al testo: comporre una premessa davanti al risultato
        // non lo puo' piu' nascondere.
        assert!(out.esito.e_fallito(), "{}", out.testo);
        // Il percorso non esiste: `resolve_relative_path` canonicalizza e
        // rifiuta prima ancora di `read_dir`. In entrambi i rami la natura e'
        // Rimediabile — l'agente scrive un'altra directory.
        assert_eq!(
            out.natura,
            Some(nexus_types::tool_outcome::NaturaFallimento::Rimediabile),
            "{}",
            out.testo
        );
    }

    /// Il gate e' BLOCCANTE: se `enforce_on_write` rifiuta, il file non deve
    /// esistere e nessun hook successivo deve partire. Mutazione che rende
    /// rosso: ignorare il valore di ritorno del gate e proseguire.
    #[tokio::test]
    async fn write_file_rifiutato_dal_gate_non_tocca_il_disco() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hooks = Arc::new(HookRegistranti {
            eventi: Mutex::new(Vec::new()),
            rifiuto: Some("\u{274C} [Errore: porta hardcoded]".to_string()),
        });
        let ctx = ctx_di_prova(dir.path().to_path_buf(), hooks.clone());

        let out = super::tool_write_file(
            &ctx,
            &serde_json::json!({ "path": "note.txt", "content": "porta 8080" }),
        )
        .await;

        // Il testo arriva dall'hook `enforce_on_write`, che NON e' ancora
        // migrato e porta il marker: `write_file` lo inoltra invariato invece
        // di riscriverlo, perche' riscrivere il messaggio di un altro
        // significherebbe interpretarlo. Il marker sparira' quando l'hook avra'
        // il suo contratto — l'esito, intanto, e' gia' nei campi.
        assert_eq!(out.testo, "\u{274C} [Errore: porta hardcoded]");
        assert_eq!(
            out.esito,
            nexus_types::tool_outcome::EsitoTool::Fallito,
            "il rifiuto del gate e' un fallimento dichiarato nel campo"
        );
        assert_eq!(
            out.natura,
            Some(nexus_types::tool_outcome::NaturaFallimento::Rimediabile),
            "l'agente puo' chiedere una porta allocata e riprovare"
        );
        assert_eq!(hooks.eventi(), vec!["enforce:write_file:note.txt".to_string()]);
        assert!(
            !dir.path().join("note.txt").exists(),
            "il gate ha rifiutato ma il file e' stato scritto lo stesso"
        );
    }

    /// `edit_file` passa dallo STESSO hook post-scrittura di `write_file`, ma
    /// senza `content`: era l'unica differenza fra `spawn_write_reindex` e
    /// `spawn_edit_reindex`, due gemelli ricopiati. Mutazione che rende rosso:
    /// passare `Some(...)` anche dal ramo edit (l'hook M2 ricreerebbe il
    /// documento da un contenuto parziale).
    /// Il fallimento di `edit_file` DICHIARA di essere rimediabile, e mantiene
    /// la promessa: il testo porta l'estratto numerato con cui correggersi.
    ///
    /// Attraversa il TOOL vero (regola O), non il tipo: e' l'unico modo di
    /// provare che la dichiarazione arriva da dove serve. Un test sul solo
    /// `RispostaTool::fallito_rimediabile` proverebbe la libreria, non l'uso.
    ///
    /// MUTAZIONE: riportando quel ramo a `RispostaTool::fallito(...)`, l'assert
    /// sulla natura rosseggia — e con essa sparisce la direttiva che il modello
    /// riceve al confine.
    #[tokio::test]
    async fn un_old_string_sbagliato_e_un_fallimento_rimediabile() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("note.txt"), "alfa
beta
gamma
").expect("seed");
        let hooks = Arc::new(HookRegistranti::default());
        let ctx = ctx_di_prova(dir.path().to_path_buf(), hooks.clone());

        let out = super::tool_edit_file(
            &ctx,
            &serde_json::json!({
                "path": "note.txt",
                "old_string": "questo testo non c'e'",
                "new_string": "x",
            }),
        )
        .await;

        assert_eq!(out.esito, nexus_types::tool_outcome::EsitoTool::Fallito);
        assert_eq!(
            out.natura,
            Some(nexus_types::tool_outcome::NaturaFallimento::Rimediabile),
            "l'agente puo' correggere da solo: va dichiarato"
        );
        // La promessa: «rimediabile» senza l'informazione per rimediare sarebbe
        // un'etichetta. L'estratto numerato del file reale e' quell'informazione.
        assert!(
            out.testo.contains("alfa"),
            "il messaggio deve portare il contenuto reale: {}",
            out.testo
        );
    }

    #[tokio::test]
    async fn edit_file_usa_lo_stesso_hook_post_scrittura_senza_contenuto() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("note.txt"), "prima\n").expect("seed");
        let hooks = Arc::new(HookRegistranti::default());
        let ctx = ctx_di_prova(dir.path().to_path_buf(), hooks.clone());

        let out = super::tool_edit_file(
            &ctx,
            &serde_json::json!({
                "path": "note.txt",
                "old_string": "prima",
                "new_string": "dopo",
            }),
        )
        .await;

        // Il test guarda i CAMPI, non il testo: e' il contratto che `edit_file`
        // ha appena adottato (regola Q).
        assert_eq!(
            out.esito,
            nexus_types::tool_outcome::EsitoTool::Riuscito,
            "edit fallito: {}",
            out.testo
        );
        assert_eq!(out.natura, None, "un successo non ha nulla da imputare");
        assert!(
            out.testo.contains("modificato con successo"),
            "testo inatteso: {}",
            out.testo
        );
        assert_eq!(
            hooks.eventi(),
            vec![
                "enforce:edit_file:note.txt".to_string(),
                "record:edit_file:note.txt".to_string(),
                "autocommit:modify:note.txt".to_string(),
                "post_write:note.txt:content=false".to_string(),
            ]
        );
    }

    #[test]
    fn is_critical_config_riconosce_i_file_di_config() {
        // B2: config che richiedono il riavvio del servizio per avere effetto.
        assert!(is_critical_config(".env"));
        assert!(is_critical_config("proj/.env.production"));
        assert!(is_critical_config("vite.config.ts"));
        assert!(is_critical_config("a/b/next.config.js"));
        assert!(is_critical_config("package.json"));
        assert!(is_critical_config("tsconfig.app.json"));
        assert!(is_critical_config("docker-compose.nexus.yml"));
        // Sorgenti normali: NON critici (niente hint inutile).
        assert!(!is_critical_config("src/app.ts"));
        assert!(!is_critical_config("src/components/Login.tsx"));
        assert!(!is_critical_config("README.md"));
        assert!(!is_critical_config("environment.ts")); // contiene "env" ma non e' .env
    }

    // ── Ricerca in-process (fallback senza grep, ramo vivo su Windows nativo) ──
    //
    // Coprono i due difetti chiusi qui: l'albero di build percorso per intero e
    // la lettura senza cap. NON coprono il fatto che `max_file_bytes` in
    // produzione arrivi da `fs_read_max_bytes` (servirebbe un ctx col DB):
    // quel collegamento e' garantito dalla firma, che non ha default.

    /// Albero di prova: la stessa riga cercabile in un sorgente, dentro un
    /// albero di build, in un dotdir e in un file grande.
    fn albero_di_ricerca(grande_bytes: usize) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let r = dir.path();
        for sub in ["src", "node_modules/pkg", "target/debug", "dist", ".git"] {
            std::fs::create_dir_all(r.join(sub)).expect("mkdir");
            std::fs::write(r.join(sub).join("f.txt"), "AGO_NEL_PAGLIAIO\n").expect("write");
        }
        std::fs::write(
            r.join("src/grande.txt"),
            format!("{}\nAGO_NEL_PAGLIAIO\n", "x".repeat(grande_bytes)),
        )
        .expect("write grande");
        dir
    }

    /// Righe dell'output senza il prefisso della tempdir: le asserzioni devono
    /// guardare i path RELATIVI, altrimenti un TMPDIR che contiene "target" o
    /// "dist" (o un suffisso random di tempfile che lo contiene) le fa fallire
    /// per l'ambiente invece che per il difetto.
    fn relativo(out: &str, dir: &tempfile::TempDir) -> String {
        out.replace(&dir.path().to_string_lossy().to_string(), "")
    }

    /// Con un `path` esplicito, cio' che `search_in_files` consegna alla ricerca
    /// non e' una stringa dell'agente: e' il prodotto di `canonicalize` dentro
    /// `resolve_relative_path`, e su Windows quel produttore antepone il
    /// prefisso verbatim `\\?\`. Il test parte da LI' (regola O) — un path
    /// scritto a mano fisserebbe proprio l'assunto da verificare — e arriva
    /// alla CONSEGUENZA, che grep trovi il file: asserire la forma del path
    /// proverebbe solo che una `replace` funziona, non che il processo
    /// dall'altra parte l'ha capita.
    ///
    /// MISURATO in esercizio il 09/08/2026 su gestione-corsi: due `agent_steps`
    /// con `status='failed'` e dentro
    /// `grep: \?D:IDEAI-projectsgestione-corsiSchoolCoursesApi: No such file or
    /// directory` — lo stesso percorso senza piu' un solo separatore.
    ///
    /// Mutazione che lo fa rosseggiare: in `run_grep_or_fallback` togliere la
    /// chiamata a `path_per_processo_esterno` e consegnare a grep il path come
    /// arriva.
    #[tokio::test]
    async fn la_ricerca_trova_il_file_partendo_da_un_path_canonicalizzato() {
        let dir = albero_di_ricerca(0);
        let canonico = dir.path().canonicalize().expect("canonicalize");

        match super::run_grep_or_fallback("AGO_NEL_PAGLIAIO", &canonico, 1_000_000).await {
            // La premessa DICHIARATA invece di un verde per assenza: dove grep
            // non e' invocabile risponde lo scanner in-process, che non passa da
            // nessun confine con un processo esterno — quel confine, qui, resta
            // semplicemente non misurato.
            super::RisultatoRicerca::RipiegoRust(_) => {
                eprintln!(
                    "premessa: `grep` non invocabile da questo processo - \
                     il confine col processo esterno NON e' stato misurato"
                );
            }
            super::RisultatoRicerca::ErroreGrep(msg) => panic!(
                "grep ha rifiutato un path canonicalizzato ({}): {msg}",
                canonico.display()
            ),
            super::RisultatoRicerca::Grep(stdout) => assert!(
                stdout.contains("AGO_NEL_PAGLIAIO"),
                "grep e' girato senza trovare nulla partendo da {}: {stdout}",
                canonico.display()
            ),
        }
    }

    /// L'altra meta' dello stesso confine: chi ha scritto l'argv e chi legge
    /// l'echo devono concordare sulla resa del path, o il prefisso della root
    /// non viene riconosciuto e l'agente riceve percorsi ASSOLUTI al posto dei
    /// relativi. Il test percorre la catena intera — ricerca reale, poi
    /// formattazione reale — invece di fabbricare le righe di grep, che
    /// fisserebbe proprio la forma da verificare (regola O).
    ///
    /// Mutazione che lo fa rosseggiare: in `formatta_risultati` tornare a
    /// strippare `root_path.to_string_lossy()` invece della sua resa.
    #[tokio::test]
    async fn le_righe_tornano_relative_anche_con_root_canonicalizzata() {
        let dir = albero_di_ricerca(0);
        let canonico = dir.path().canonicalize().expect("canonicalize");

        let stdout =
            match super::run_grep_or_fallback("AGO_NEL_PAGLIAIO", &canonico, 1_000_000).await {
                super::RisultatoRicerca::Grep(s) | super::RisultatoRicerca::RipiegoRust(s) => s,
                super::RisultatoRicerca::ErroreGrep(msg) => panic!("ricerca fallita: {msg}"),
            };
        let reso = super::formatta_risultati(&canonico, "AGO_NEL_PAGLIAIO", &stdout);

        assert!(
            reso.contains("AGO_NEL_PAGLIAIO"),
            "la ricerca non ha prodotto righe: {reso}"
        );
        // Le due forme assolute della stessa root. La seconda passa da
        // `path_for_storage`, funzione INDIPENDENTE da quella sotto esame: usare
        // qui `path_per_processo_esterno` renderebbe l'asserzione circolare.
        let assoluto_verbatim = canonico.to_string_lossy().to_string();
        let assoluto_pulito = nexus_types::workspace_paths::path_for_storage(&canonico);
        assert!(
            !reso.contains(&assoluto_verbatim) && !reso.contains(&assoluto_pulito),
            "le righe devono essere relative alla root, nessuna forma assoluta puo' restare.\n\
             verbatim: {assoluto_verbatim}\npulito:   {assoluto_pulito}\nreso: {reso}"
        );
    }

    #[test]
    fn ricerca_salta_gli_alberi_di_build_e_i_dotdir() {
        let dir = albero_di_ricerca(0);
        // Cap alto: qui si misura solo quali DIRECTORY vengono percorse.
        let out = super::search_in_files_rust(dir.path(), "AGO_NEL_PAGLIAIO", 1_000_000);
        let rel = relativo(&out, &dir);

        assert!(rel.contains("src"), "il sorgente va trovato: {rel}");
        // Mutazione che rende rosso: rimettere `name.starts_with('.')` al posto
        // di `is_skipped_dir` -> questi tre alberi tornano nei risultati.
        assert!(
            !rel.contains("node_modules"),
            "node_modules non va percorso: {rel}"
        );
        assert!(!rel.contains("target"), "target non va percorso: {rel}");
        assert!(!rel.contains("dist"), "dist non va percorso: {rel}");
        // Comportamento storico preservato dalla delega: i dotdir restano fuori.
        assert!(!rel.contains(".git"), "i dotdir restano esclusi: {rel}");
    }

    #[test]
    fn ricerca_salta_i_file_oltre_il_cap_senza_leggerli() {
        let dir = albero_di_ricerca(4096);
        let out = super::search_in_files_rust(dir.path(), "AGO_NEL_PAGLIAIO", 1024);

        // Mutazione che rende rosso: togliere il controllo su `metadata()` in
        // append_file_matches -> il file da 4 KB viene letto e il match appare.
        assert!(
            !out.contains("grande.txt"),
            "il file oltre il cap non va nemmeno letto: {out}"
        );
        assert!(
            out.contains("f.txt"),
            "i file sotto il cap restano cercabili: {out}"
        );
    }

    /// Il cap non e' un numero scritto nella ricerca: viene dalla governance.
    /// Senza questo test nulla proverebbe QUALE chiave viene letta - un refuso
    /// nel nome farebbe cadere ogni lettura sul default, in silenzio.
    #[sqlx::test]
    async fn il_cap_viene_dalla_chiave_di_governance(pool: sqlx::PgPool) {
        sqlx::query("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .execute(&pool)
            .await
            .expect("create table settings");

        // Chiave assente: default dichiarato (2 MB), lo stesso di read_file.
        assert_eq!(super::fs_read_max_bytes(&pool).await, 2_097_152);

        sqlx::query("INSERT INTO settings (key, value) VALUES ('agent.fs.read_max_bytes', '4096')")
            .execute(&pool)
            .await
            .expect("insert setting");
        // Scrittura diretta: la lettura passa dalla cache dei settings, il test
        // invalida come farebbe una sessione esterna (o lo scadere del TTL).
        nexus_auth::invalidate_setting_cache(&pool, "agent.fs.read_max_bytes");

        // Mutazione che rende rosso: cambiare il nome della chiave letta in
        // fs_read_max_bytes -> torna il default e questa asserzione cade.
        assert_eq!(super::fs_read_max_bytes(&pool).await, 4096);
    }

    #[test]
    fn una_ricerca_esplicita_dentro_un_albero_escluso_funziona() {
        let dir = albero_di_ricerca(0);
        // Chi cerca DENTRO node_modules deve trovare: la skip-list filtra i nomi
        // delle entry visitate, mai la root da cui si parte. E' anche il motivo
        // per cui il ramo grep non riceve `--exclude-dir`, che invece filtra
        // pure l'operando da riga di comando.
        let out = super::search_in_files_rust(
            &dir.path().join("node_modules"),
            "AGO_NEL_PAGLIAIO",
            1_000_000,
        );

        assert!(
            out.contains("f.txt"),
            "la root della ricerca non va filtrata: {out}"
        );
    }

    #[test]
    fn cap_a_zero_disattiva_il_limite_di_dimensione() {
        let dir = albero_di_ricerca(4096);
        // 0 = setting disattivato: stessa semantica di agent.fs.read_max_bytes.
        let out = super::search_in_files_rust(dir.path(), "AGO_NEL_PAGLIAIO", 0);

        assert!(
            out.contains("grande.txt"),
            "con cap 0 il file grande torna cercabile: {out}"
        );
    }

    fn make_file(num_lines: usize) -> String {
        (1..=num_lines)
            .map(|i| match i {
                42 => "pub fn target_function(arg: u32) -> u32 { arg + 1 }".to_string(),
                _ => format!("// riga di riempimento numero {i}"),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn estratto_numerato_attorno_alla_riga_simile() {
        let content = make_file(120);
        // Old string del modello stantio: cerca la firma vecchia che NON
        // matchera' piu' (per simulare l'edit cieco).
        let old_string = "pub fn target_function(arg: u32) -> u32 { arg + 2 }";

        let msg = build_old_string_not_found_message(&content, old_string, "src/lib.rs");

        // 1. Messaggio originale conservato.
        assert!(
            msg.starts_with("\u{274C} [Errore: old_string non trovato nel file 'src/lib.rs'."),
            "header originale non preservato: {}",
            msg
        );
        assert!(
            msg.contains("NON chiamare read_file"),
            "warning anti-loop perso: {}",
            msg
        );

        // 2. Riga simile correttamente individuata (riga 42).
        assert!(
            msg.contains("Prima riga simile trovata ~riga 42."),
            "ancora di navigazione non presente: {}",
            msg
        );

        // 3. Header dell'estratto presente con riferimento alla riga 42.
        assert!(
            msg.contains("Contenuto attuale attorno alla riga 42"),
            "header dell'estratto attorno alla riga simile mancante: {}",
            msg
        );

        // 4. Estratto NUMERATO contiene la riga 42 ed alcune righe attorno
        //    (finestra +/- 15: dovrebbe coprire almeno 27 e 57).
        assert!(
            msg.contains("  42 | pub fn target_function"),
            "riga 42 numerata non presente nell'estratto: {}",
            msg
        );
        assert!(
            msg.contains("  30 | "),
            "riga 30 (window before) attesa nell'estratto: {}",
            msg
        );
        assert!(
            msg.contains("  55 | "),
            "riga 55 (window after) attesa nell'estratto: {}",
            msg
        );

        // 5. Limite di sicurezza: il messaggio totale deve restare contenuto
        //    (tetto ~2 KB sull'estratto + margine).
        assert!(
            msg.len() < 4096,
            "messaggio sopra il tetto ragionevole ({}B): {}",
            msg.len(),
            msg
        );
    }

    #[test]
    fn fallback_alle_prime_righe_se_nessun_token_simile() {
        let content = "alpha\nbeta\ngamma\ndelta\n".to_string();
        let old_string = "stringa_che_non_compare_da_nessuna_parte_xyz123";

        let msg = build_old_string_not_found_message(&content, old_string, "f.txt");

        assert!(
            msg.contains("Nessuna riga contiene il primo token di old_string."),
            "hint di assenza atteso: {}",
            msg
        );
        // L'estratto di fallback parte dalla riga 1.
        assert!(
            msg.contains("   1 | alpha"),
            "fallback alle prime righe non emesso: {}",
            msg
        );
    }

    #[test]
    fn occurrence_start_lines_localizza_le_occorrenze() {
        let content = "fn a() {}\nlet x = foo();\nfn b() {}\nlet y = foo();\n";
        let hits = occurrence_start_lines(content, "foo()", 5);
        // foo() compare a riga 2 (idx 1) e riga 4 (idx 3).
        assert_eq!(hits, vec![1, 3]);
        // Cap rispettato.
        let capped = occurrence_start_lines(content, "foo()", 1);
        assert_eq!(capped, vec![1]);
        // needle vuoto -> nessun hit (niente loop).
        assert!(occurrence_start_lines(content, "", 5).is_empty());
    }

    #[test]
    fn scrittura_che_non_cambia_nulla_lo_dichiara() {
        // Successo SILENZIOSO: il tool riesce, quindi nessun rilevatore di errori
        // lo nota, ma il file resta identico. E' il caso che alimenta la
        // non-convergenza (incidente 2026-07-22: 28 operazioni sullo stesso file,
        // sub-run ucciso dal timeout senza mai avanzare). Il messaggio deve dirlo
        // esplicitamente, perche' dall'esito strutturato "ok" il modello non puo'
        // distinguere questo da un progresso reale.
        let noop = build_write_success_message("src/a.rs", 120, true, None);
        assert!(noop.contains("IDENTICO"), "manca il segnale di no-op: {noop}");
        assert!(noop.contains("NON ha modificato nulla"), "{noop}");

        // Scrittura che cambia davvero: nessun falso allarme.
        let reale = build_write_success_message("src/a.rs", 120, false, None);
        assert!(
            !reale.contains("IDENTICO"),
            "falso allarme su una scrittura reale: {reale}"
        );
        assert!(reale.contains("scritto con successo"));
    }

    #[test]
    fn ramo_ambiguo_include_estratto_numerato() {
        // old_string presente 3 volte: il messaggio deve mostrare l'estratto
        // numerato delle prime occorrenze, NON solo testo generico.
        let content = "header\nval = compute();\nmid1\nmid2\nval = compute();\nmid3\nval = compute();\nfooter\n";
        let old_string = "val = compute();";

        let msg = build_old_string_ambiguous_message(content, old_string, "src/lib.rs", 3);

        // Header informa del conteggio e dell'obbligo di univocita'.
        assert!(
            msg.contains("trovato 3 volte in 'src/lib.rs'"),
            "conteggio mancante: {}",
            msg
        );
        assert!(
            msg.contains("deve essere UNICO"),
            "vincolo univocita' mancante: {}",
            msg
        );
        // Anti-loop: niente read_file, il contesto e' gia' qui.
        assert!(
            msg.contains("NON chiamare read_file"),
            "anti-loop mancante: {}",
            msg
        );
        // Estratto numerato presente con la riga reale dell'occorrenza.
        assert!(
            msg.contains("| val = compute();"),
            "riga numerata dell'occorrenza mancante: {}",
            msg
        );
        // Etichette di occorrenza multipla.
        assert!(
            msg.contains("Occorrenza 1"),
            "etichetta occorrenza 1 mancante: {}",
            msg
        );
        assert!(
            msg.contains("Occorrenza 2"),
            "etichetta occorrenza 2 mancante: {}",
            msg
        );
    }

    /// I tre tool che mutano il filesystem dichiarano l'esito nei CAMPI, e la
    /// natura del fallimento la prendono dal `ErrorKind` invece che sceglierla
    /// caso per caso.
    ///
    /// MUTAZIONE: sostituendo `da_errore_io` con un `fallito_rimediabile`
    /// fisso, il ramo del file inesistente resta verde ma quello del permesso
    /// negato mentirebbe — ed e' il motivo per cui la natura non si sceglie a
    /// mano su un errore che il sistema operativo ha gia' classificato.
    /// La causa del percorso deve arrivare fino al MODELLO, che e' il solo
    /// consumatore di questo testo (regola O: si asserisce la conseguenza, non
    /// la stringa della funzione interna — `paths.rs` prova gia' quella).
    ///
    /// Il caso e' quello misurato l'08/08/2026 su gestione-corsi: il modello
    /// chiede un sottopath di una cartella che ESISTE. Il testo storico era
    /// "[Errore percorso: Percorso non trovato]" per QUALUNQUE causa, e da li'
    /// non si poteva sapere se avesse sbagliato l'ultimo segmento, l'intero
    /// ramo, o se il resolver fosse rotto.
    #[tokio::test]
    async fn list_files_consegna_al_modello_la_causa_e_il_tratto_esistente() {
        use nexus_types::tool_outcome::EsitoTool;

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("school-courses-fe/src")).expect("albero");
        let ctx = ctx_di_prova(dir.path().to_path_buf(), Arc::new(HookRegistranti::default()));

        let out = super::tool_list_files(
            &ctx,
            &serde_json::json!({"directory": "school-courses-fe/SchoolCoursesApi"}),
        )
        .await;

        // Il tool e' stato migrato a `RispostaTool` mentre questo test viveva su
        // un altro branch: l'asserzione guarda ora i CAMPI, che e' il contratto
        // che il tool ha adottato (regola Q). Anche questo assert porta la sua
        // diagnostica, e la porta sulla struttura INTERA (`{out:?}`): `assert_eq!`
        // da solo stampa i due `esito` e tace su `natura` e `testo`, che sono i
        // campi da cui si capisce PERCHE' l'esito non e' quello atteso.
        assert_eq!(out.esito, EsitoTool::Fallito, "{out:?}");
        assert!(
            out.testo.contains("school-courses-fe/SchoolCoursesApi"),
            "{out:?}"
        );
        assert!(
            out.testo
                .contains("il tratto esistente piu' profondo e' 'school-courses-fe'"),
            "il modello deve sapere fin dove il percorso esiste: {out:?}"
        );
    }

    #[tokio::test]
    async fn i_tool_di_mutazione_dichiarano_esito_e_natura_nei_campi() {
        use nexus_types::tool_outcome::{EsitoTool, NaturaFallimento};

        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_di_prova(dir.path().to_path_buf(), Arc::new(HookRegistranti::default()));

        // Creare una directory NUOVA: e' l'unica cosa che questo tool fa, ed e'
        // il caso in cui non funzionava. Con `resolve_relative_path` il
        // percorso veniva canonicalizzato prima di esistere e il tool rispondeva
        // "[Errore percorso: Percorso non trovato]" — `create_dir_all` era
        // irraggiungibile, e il solo esito possibile era "esiste gia'".
        // Rimettendo quel resolver, questa riga rosseggia.
        let out = super::tool_fs_mkdir(&ctx, &serde_json::json!({"path": "sotto"})).await;
        assert_eq!(out.esito, EsitoTool::Riuscito, "{}", out.testo);
        assert!(dir.path().join("sotto").is_dir(), "la directory esiste sul disco");
        let out = super::tool_fs_mkdir(&ctx, &serde_json::json!({"path": "sotto"})).await;
        assert_eq!(out.esito, EsitoTool::Riuscito, "esiste gia' non e' un errore");

        // Un parametro mancante lo ferma il CONTRATTO, non un controllo scritto
        // a mano dentro l'handler: la natura e' rimediabile e il testo nomina
        // il tool.
        let out = super::tool_rename_file(&ctx, &serde_json::json!({"from": "a.txt"})).await;
        assert_eq!(out.esito, EsitoTool::Fallito);
        assert_eq!(out.natura, Some(NaturaFallimento::Rimediabile));
        assert!(out.testo.contains("rename_file"), "{}", out.testo);

        // Eliminare cio' che non c'e': l'errore e' `NotFound`, quindi
        // rimediabile — l'agente puo' verificare il percorso.
        let out =
            super::tool_delete_file(&ctx, &serde_json::json!({"path": "mai_esistito.txt"})).await;
        assert_eq!(out.esito, EsitoTool::Fallito, "{}", out.testo);
        assert_eq!(out.natura, Some(NaturaFallimento::Rimediabile));

        // Copiare verso una destinazione NUOVA: stesso difetto di mkdir, in un
        // altro punto. `resolve_from_to` canonicalizzava anche la destinazione,
        // quindi copia e spostamento riuscivano solo verso un percorso gia'
        // esistente — con `overwrite:true`, l'unico caso che il controllo
        // `to.exists() && !overwrite` lascia passare. Rimettendo li'
        // `resolve_relative_path`, questa riga rosseggia.
        std::fs::write(dir.path().join("sorgente.txt"), "dati").expect("sorgente");
        let out = super::tool_fs_copy(
            &ctx,
            &serde_json::json!({"from": "sorgente.txt", "to": "copia.txt"}),
        )
        .await;
        assert_eq!(
            out.esito,
            EsitoTool::Riuscito,
            "copia verso una destinazione nuova: {}",
            out.testo
        );
        assert!(dir.path().join("copia.txt").is_file(), "la copia esiste");

        // Senza permesso di scrittura la causa e' del SISTEMA: e' una decisione
        // del progetto, e ripetere non la cambia.
        let mut senza_permesso =
            ctx_di_prova(dir.path().to_path_buf(), Arc::new(HookRegistranti::default()));
        senza_permesso.can_write = false;
        let out =
            super::tool_delete_file(&senza_permesso, &serde_json::json!({"path": "x"})).await;
        assert_eq!(out.natura, Some(NaturaFallimento::DelSistema), "{}", out.testo);
    }

    /// `read_file_lines` prende gli estremi dal CONTRATTO, e ogni modo di
    /// sbagliarli e' un fallimento rimediabile il cui testo dice come.
    ///
    /// La riga che conta e' quella degli alias: `offset`/`limit` erano mappati
    /// in silenzio a `start_line`/`end_line` mentre il catalogo non li dichiara
    /// e il prompt del supervisore (mig 0060) dice al modello che NON esistono.
    /// Il contratto chiude quella divergenza — e il modello che li usa comunque
    /// riceve un messaggio che nomina il campo, non una lettura di righe che non
    /// aveva chiesto.
    ///
    /// MUTAZIONE: rimettendo la mappatura degli alias, l'asserzione sul
    /// fallimento di `offset`/`limit` rosseggia; togliendo il totale dal
    /// messaggio di `start_line` oltre il file, rosseggia quella sul "10" —
    /// che e' l'informazione senza cui "rimediabile" sarebbe una promessa non
    /// mantenuta.
    #[tokio::test]
    async fn read_file_lines_prende_l_intervallo_dal_contratto() {
        use nexus_types::tool_outcome::{EsitoTool, NaturaFallimento};

        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_di_prova(dir.path().to_path_buf(), Arc::new(HookRegistranti::default()));
        let righe: String = (1..=10).map(|n| format!("riga {n}\n")).collect();
        std::fs::write(dir.path().join("f.txt"), righe).expect("seed");

        let out = super::tool_read_file_lines(
            &ctx,
            &serde_json::json!({"path": "f.txt", "start_line": 3, "end_line": 4}),
        )
        .await;
        assert_eq!(out.esito, EsitoTool::Riuscito, "{}", out.testo);
        assert!(out.testo.contains("riga 3"), "{}", out.testo);
        assert!(out.testo.contains("riga 4"), "{}", out.testo);
        assert!(!out.testo.contains("riga 5"), "estremo destro incluso: {}", out.testo);

        // Gli alias che il catalogo non promette: li ferma il contratto, e il
        // messaggio nomina il tool e il campo sconosciuto.
        let out = super::tool_read_file_lines(
            &ctx,
            &serde_json::json!({"path": "f.txt", "offset": 3, "limit": 2}),
        )
        .await;
        assert_eq!(out.esito, EsitoTool::Fallito, "{}", out.testo);
        assert_eq!(out.natura, Some(NaturaFallimento::Rimediabile));
        assert!(out.testo.contains("read_file_lines"), "{}", out.testo);

        // Estremi fuori dominio: `i64` li fa arrivare fino al controllo, che dice
        // QUALE estremo e' sbagliato invece di fallire nel deserializzatore.
        let out = super::tool_read_file_lines(
            &ctx,
            &serde_json::json!({"path": "f.txt", "start_line": 0, "end_line": 4}),
        )
        .await;
        assert_eq!(out.natura, Some(NaturaFallimento::Rimediabile), "{}", out.testo);
        assert!(out.testo.contains("start_line"), "{}", out.testo);

        let out = super::tool_read_file_lines(
            &ctx,
            &serde_json::json!({"path": "f.txt", "start_line": 8, "end_line": 2}),
        )
        .await;
        assert_eq!(out.natura, Some(NaturaFallimento::Rimediabile), "{}", out.testo);
        assert!(out.testo.contains("end_line"), "{}", out.testo);

        // Oltre la fine del file: il messaggio porta il totale, che e' cio' con
        // cui l'agente costruisce un intervallo valido.
        let out = super::tool_read_file_lines(
            &ctx,
            &serde_json::json!({"path": "f.txt", "start_line": 99, "end_line": 120}),
        )
        .await;
        assert_eq!(out.natura, Some(NaturaFallimento::Rimediabile), "{}", out.testo);
        assert!(out.testo.contains("10"), "manca il totale righe: {}", out.testo);

        // File assente: la natura la decide il `ErrorKind`, non una scelta
        // scritta a mano in questo handler.
        let out = super::tool_read_file_lines(
            &ctx,
            &serde_json::json!({"path": "mai_esistito.txt", "start_line": 1, "end_line": 2}),
        )
        .await;
        assert_eq!(out.esito, EsitoTool::Fallito, "{}", out.testo);
        assert_eq!(out.natura, Some(NaturaFallimento::Rimediabile));
    }

    /// Il pattern mancante di `search_in_files` lo ferma il contratto, PRIMA di
    /// qualunque I/O: il tool non arriva a leggere il cap di governance dal DB.
    ///
    /// E' anche la ragione per cui questo test e' istantaneo su un pool lazy mai
    /// connesso — la stessa proprieta' che nel dispatcher ha portato un test da
    /// 150 secondi a zero.
    #[tokio::test]
    async fn search_in_files_senza_pattern_lo_ferma_il_contratto() {
        use nexus_types::tool_outcome::NaturaFallimento;

        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_di_prova(dir.path().to_path_buf(), Arc::new(HookRegistranti::default()));

        let out = super::tool_search_in_files(&ctx, &serde_json::json!({})).await;

        assert!(out.esito.e_fallito(), "{}", out.testo);
        assert_eq!(out.natura, Some(NaturaFallimento::Rimediabile));
        assert!(out.testo.contains("search_in_files"), "{}", out.testo);
    }

    /// `fs_move` dichiara nei campi i tre esiti che lo distinguono da `fs_copy`.
    ///
    /// MUTAZIONE: dichiarando `fallito_di_sistema` la destinazione occupata,
    /// l'agente riceve «cambia strada» per una condizione che risolve da solo
    /// scegliendo un altro nome, e la riga sulla natura rosseggia.
    #[tokio::test]
    async fn fs_move_dichiara_esito_e_natura_nei_campi() {
        use nexus_types::tool_outcome::{EsitoTool, NaturaFallimento};

        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ctx_di_prova(dir.path().to_path_buf(), Arc::new(HookRegistranti::default()));
        std::fs::write(dir.path().join("a.txt"), "dati").expect("seed");
        std::fs::write(dir.path().join("occupata.txt"), "altro").expect("seed");

        // Verso una destinazione NUOVA, dentro una directory che non esiste
        // ancora: il genitore lo crea il tool.
        let out = super::tool_fs_move(
            &ctx,
            &serde_json::json!({"from": "a.txt", "to": "sotto/b.txt"}),
        )
        .await;
        assert_eq!(out.esito, EsitoTool::Riuscito, "{}", out.testo);
        assert!(dir.path().join("sotto/b.txt").is_file(), "spostato sul disco");
        assert!(!dir.path().join("a.txt").exists(), "la sorgente non resta");

        // Destinazione gia' occupata: rimediabile, l'agente ne sceglie un'altra.
        let out = super::tool_fs_move(
            &ctx,
            &serde_json::json!({"from": "sotto/b.txt", "to": "occupata.txt"}),
        )
        .await;
        assert_eq!(out.esito, EsitoTool::Fallito, "{}", out.testo);
        assert_eq!(out.natura, Some(NaturaFallimento::Rimediabile));

        // Senza permesso di scrittura la decisione e' del progetto: ripetere non
        // la cambia, e il contratto non viene nemmeno letto.
        let mut senza_permesso =
            ctx_di_prova(dir.path().to_path_buf(), Arc::new(HookRegistranti::default()));
        senza_permesso.can_write = false;
        let out = super::tool_fs_move(
            &senza_permesso,
            &serde_json::json!({"from": "sotto/b.txt", "to": "c.txt"}),
        )
        .await;
        assert_eq!(out.natura, Some(NaturaFallimento::DelSistema), "{}", out.testo);
    }
}

#[cfg(test)]
mod tests_ancora_estratto {
    use super::{ancora_da_prefisso, ancora_per_sovrapposizione};

    /// Il file che ha prodotto il difetto, ridotto: JSX con `<div className=`
    /// ripetuto. L'old_string sta in FONDO.
    pub(super) fn file_jsx() -> Vec<&'static str> {
        vec![
            "import { useState } from 'react';",          // 0
            "",                                            // 1
            "function App() {",                            // 2
            "  return (",                                  // 3
            "    <div className=\"min-h-screen bg-gray-50\">",  // 4  <- prima occorrenza
            "      <header className=\"bg-blue-600\">",         // 5
            "        <h1>Biblioteca</h1>",                  // 6
            "      </header>",                              // 7
            "      <div className=\"container\">",             // 8  <- seconda
            "        <BookCatalog />",                      // 9
            "      </div>",                                 // 10
            "    </div>",                                   // 11
            "  );",                                         // 12
            "}",                                            // 13
            "",                                             // 14
            "function BookCatalog() {",                     // 15
            "  return (",                                   // 16
            "    <div className=\"mt-4\">",                   // 17 <- QUI sta l'old_string
            "      <h3>Elenco Libri</h3>",                   // 18
            "      <ul className=\"space-y-2\">",             // 19
            "        {books.map((book) => (",                // 20
            "          <li key={book.id}>{book.title}</li>", // 21
            "        ))}",                                   // 22
            "      </ul>",                                   // 23
            "    </div>",                                    // 24
            "  );",                                          // 25
            "}",                                             // 26
        ]
    }

    /// MUTAZIONE: ripristinando l'ancora al primo-token (cioe' facendo tornare
    /// `ancora_per_sovrapposizione` a `None` e lasciando decidere il prefisso),
    /// questo test rosseggia con l'indice 4 al posto del 17 — esattamente il
    /// difetto misurato: l'estratto mostrava la testa del file mentre
    /// l'old_string stava in fondo.
    #[test]
    fn ancora_la_zona_giusta_anche_se_la_prima_riga_e_ambigua() {
        let lines = file_jsx();
        let old = "    <div className=\"mt-4\">
      <h3>Elenco Libri</h3>
      <ul className=\"space-y-2\">
";
        let i = ancora_per_sovrapposizione(&lines, old).expect("almeno una riga in comune");
        assert!(
            (16..=18).contains(&i),
            "l'ancora deve cadere sul blocco di BookCatalog (~riga 17), non sulla              prima <div className> del file: ottenuto {i}"
        );

        // E si dimostra che il criterio VECCHIO sbagliava, sullo stesso input.
        let vecchia = ancora_da_prefisso(&lines, "<div className");
        assert_eq!(
            vecchia,
            Some(4),
            "il prefisso da solo prende la PRIMA delle tre <div className>:              e' la ragione per cui l'agente riceveva la parte sbagliata del file"
        );
    }

    /// LO STESSO CASO, ma attraverso il PRODUTTORE REALE del messaggio
    /// (regola O): i test qui sopra chiamano la funzione d'ancoraggio in
    /// isolamento, quindi restano verdi anche se qualcuno la scollega dal call
    /// site — verificato provandolo. Questo test parte da dove parte la
    /// produzione, cioe' dal testo che l'agente si vede arrivare.
    ///
    /// MUTAZIONE: sostituendo il criterio con il solo prefisso nel call site di
    /// `build_old_string_not_found_message`, questo rosseggia perche' l'estratto
    /// mostra l'intestazione del file al posto del blocco cercato.
    #[test]
    fn il_messaggio_mostra_la_zona_dove_cercava_l_agente() {
        let contenuto = super::super::files::tests_ancora_estratto::file_jsx().join("
");
        let old = "    <div className=\"mt-4\">
      <h3>Elenco Libri</h3>
      <ul className=\"space-y-2\">
";
        let msg = super::build_old_string_not_found_message(&contenuto, old, "frontend/src/App.jsx");

        assert!(
            msg.contains("Elenco Libri"),
            "l'estratto deve contenere la zona che l'agente cercava.
--- messaggio ---
{msg}"
        );
        assert!(
            !msg.contains("import { useState }"),
            "l'estratto NON deve essere l'intestazione del file: e' cio' che              l'agente riceveva mentre cercava un blocco in fondo.
--- messaggio ---
{msg}"
        );
    }

    /// Il caso per cui `anchor_prefix` era nato: firma invariata, corpo
    /// riscritto. Nessuna riga in comune -> il ripiego deve restare utile.
    #[test]
    fn senza_righe_in_comune_ripiega_sul_prefisso() {
        let lines = vec![
            "fn altra() {}",
            "pub fn target_function(arg: u32) -> u32 {",
            "    arg + 1",
            "}",
        ];
        // old_string col corpo diverso: nessuna riga coincide interamente.
        let old = "pub fn target_function(arg: u32) -> u32 {
    arg + 999
}
";
        let per_sovrapposizione = ancora_per_sovrapposizione(&lines, old);
        // La graffa di chiusura `}` coincide: la sovrapposizione la trova ed e'
        // gia' la zona giusta.
        assert!(per_sovrapposizione.is_some());
        // Il ripiego, se servisse, punta comunque alla firma.
        assert_eq!(ancora_da_prefisso(&lines, "pub fn target_function(arg"), Some(1));
    }

    /// Nessuna riga in comune e nessun prefisso: si dichiara l'assenza invece
    /// di indicare una zona a caso. Chi legge l'errore deve poter capire che il
    /// file e' diverso da quello che immaginava.
    #[test]
    fn file_del_tutto_diverso_non_inventa_un_ancora() {
        let lines = vec!["alpha", "beta", "gamma"];
        assert_eq!(ancora_per_sovrapposizione(&lines, "delta
epsilon
"), None);
        assert_eq!(ancora_da_prefisso(&lines, ""), None);
    }

    /// A parita' di sovrapposizione vince la finestra piu' in alto: due blocchi
    /// identici non devono dare un'ancora che cambia da un'esecuzione all'altra.
    #[test]
    fn a_parita_di_punteggio_vince_la_prima_finestra() {
        let lines = vec!["a", "x", "y", "b", "x", "y", "c"];
        let i = ancora_per_sovrapposizione(&lines, "x
y
").expect("righe in comune");
        assert!(i <= 2, "atteso il primo blocco, ottenuto {i}");
    }
}
