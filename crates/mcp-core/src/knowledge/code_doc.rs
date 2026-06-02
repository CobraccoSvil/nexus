//! W2 code-wiki: generazione di documentazione AI per-file.
//!
//! Per ogni file di codice produce una nota `kind='code_doc'` (title = path
//! relativo) con: header strutturale (linguaggio, simboli, import estratti da
//! `mcp_ast`, W1, language-agnostic) + spiegazione generata dall'LLM. Riusa
//! `apply_project_note` per UPSERT idempotente (project_id, kind, title) +
//! embedding + Qdrant, cosi' la doc e' ricercabile e linkabile come ogni nota.
//!
//! Niente modelli hardcoded (regola G): il modello viene da
//! `nexus_purpose_model` purpose 'code_doc'. Se non configurato, errore esplicito.

use crate::knowledge::generators::{apply_project_note, GeneratedProjectNote};
use crate::AppState;
use anyhow::{Context, Result};
use axum::{
    extract::{Extension, Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use mcp_ast::index_source;
use nexus_types::{api_error, ensure_project_access, parse_user_id, ApiResult};
use serde_json::json;
use std::path::Path;
use uuid::Uuid;

use crate::auth::Claims;

async fn setting_i64(state: &AppState, key: &str, default: i64) -> i64 {
    crate::settings::get_setting(&state.db, key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(default)
}

async fn setting_bool(state: &AppState, key: &str, default: bool) -> bool {
    crate::settings::get_setting(&state.db, key)
        .await
        .ok()
        .flatten()
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on"))
        .unwrap_or(default)
}

/// Risolve (provider, model) per il purpose 'code_doc'. Nessun fallback
/// hardcoded: propaga errore se la routing matrix non e' disponibile o il
/// purpose non e' configurato (regola G).
async fn resolve_code_doc_model(state: &AppState) -> Result<(String, String)> {
    let matrix = state
        .orchestrator
        .routing_matrix
        .current_async()
        .await
        .map_err(|e| anyhow::anyhow!("routing matrix non disponibile: {e}"))?;
    matrix
        .purpose_model("code_doc")
        .ok_or_else(|| anyhow::anyhow!("purpose 'code_doc' non configurato in nexus_purpose_model"))
}

/// Genera (o aggiorna) la nota code_doc per un singolo file.
pub async fn generate_code_doc_for_file(
    state: &AppState,
    project_id: Uuid,
    rel_path: &str,
    content: &str,
    provider: &str,
    model: &str,
) -> Result<Uuid> {
    // 1. Struttura language-agnostic (W1).
    let ast = index_source(rel_path, content);

    // 2. Prompt (sorgente troncato a una soglia DB-driven).
    let max_chars = setting_i64(state, "kb.code_doc.max_source_chars", 12000).await as usize;
    let snippet: String = content.chars().take(max_chars).collect();
    let symbols_list = ast
        .symbols
        .iter()
        .take(80)
        .map(|s| format!("- {:?} `{}` (riga {}, {:?})", s.kind, s.name, s.line, s.visibility))
        .collect::<Vec<_>>()
        .join("\n");
    let imports_list = ast
        .imports
        .iter()
        .take(40)
        .map(|i| format!("- {}", i.module))
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        "Sei un documentatore tecnico. Documenta in italiano, in Markdown, il file di codice qui sotto, \
         per una wiki del codice consultata da sviluppatori. Usa ESATTAMENTE queste sezioni:\n\
         ## Scopo\n(2-4 frasi: cosa fa il file e perche' esiste)\n\
         ## Componenti principali\n(elenco dei simboli rilevanti con il loro ruolo)\n\
         ## Dipendenze e relazioni\n(da cosa dipende, chi lo usa, se deducibile)\n\
         ## Note per chi modifica\n(insidie, invarianti, effetti collaterali)\n\n\
         Sii conciso e accurato. NON inventare: se un'informazione non e' deducibile dal codice, ometti la voce.\n\n\
         File: {path}\nLinguaggio: {lang}\n\nSimboli rilevati:\n{symbols}\n\nImport rilevati:\n{imports}\n\n\
         --- CONTENUTO ({nchars} char, troncato se necessario) ---\n{snippet}",
        path = rel_path,
        lang = ast.language,
        symbols = if symbols_list.is_empty() { "(nessuno rilevato)".to_string() } else { symbols_list },
        imports = if imports_list.is_empty() { "(nessuno rilevato)".to_string() } else { imports_list },
        nchars = content.chars().count(),
        snippet = snippet,
    );

    // 3. Generazione LLM.
    let resp = state
        .orchestrator
        .neural
        .generate_completion(provider, model, &prompt)
        .await
        .context("generate_completion code_doc")?;
    let doc = resp
        .get("content")
        .and_then(|v| v.as_str())
        .or_else(|| resp.get("text").and_then(|v| v.as_str()))
        .or_else(|| resp.get("message").and_then(|v| v.as_str()))
        .unwrap_or("")
        .trim()
        .to_string();
    if doc.is_empty() || doc.to_lowercase().starts_with("[error") || doc.starts_with("Errore del provider") {
        anyhow::bail!("documentazione vuota o errore provider per {rel_path}");
    }

    // 4a. Diagramma Mermaid delle dipendenze, generato deterministicamente dagli
    //     import (piu' affidabile che farlo produrre all'LLM). Reso dal frontend
    //     (W3). Gli id dei nodi sono sintetici; i path vanno nelle label.
    let mermaid = if ast.imports.is_empty() {
        String::new()
    } else {
        let mut lines = vec!["graph LR".to_string()];
        let self_label = rel_path.replace('"', "'");
        lines.push(format!("    self[\"{}\"]", self_label));
        for (i, imp) in ast.imports.iter().take(15).enumerate() {
            let label = imp.module.replace('"', "'");
            lines.push(format!("    self --> dep{i}[\"{label}\"]"));
        }
        format!(
            "\n\n## Dipendenze (diagramma)\n\n```mermaid\n{}\n```\n",
            lines.join("\n")
        )
    };

    // 4b. Body: header strutturale (deterministico) + spiegazione AI + diagramma.
    let body = format!(
        "<!-- code_doc: generato automaticamente da Nexus, non modificare a mano -->\n\n\
         # `{path}`\n\n\
         *Linguaggio: **{lang}** — {nsym} simboli, {nimp} import, {nlines} righe*\n\n\
         {doc}{mermaid}",
        path = rel_path,
        lang = ast.language,
        nsym = ast.symbols.len(),
        nimp = ast.imports.len(),
        nlines = ast.line_count,
        doc = doc,
        mermaid = mermaid,
    );

    let note = GeneratedProjectNote {
        kind: "code_doc".to_string(),
        title: rel_path.to_string(),
        body_md: body,
        intent: Some("code_doc".to_string()),
        tags: vec!["kind:code_doc".to_string(), format!("lang:{}", ast.language)],
        file_paths: vec![rel_path.to_string()],
    };
    apply_project_note(state, project_id, &note).await
}

/// Genera la code-wiki per l'intero progetto: itera sui file noti in
/// `project_code_nodes`, legge il contenuto dal filesystem e genera la doc.
/// Ritorna (generati, saltati). Gated da `kb.code_doc.enabled`.
pub async fn generate_code_wiki_for_project(
    state: &AppState,
    project_id: Uuid,
) -> Result<(usize, usize)> {
    if !setting_bool(state, "kb.code_doc.enabled", true).await {
        anyhow::bail!("kb.code_doc.enabled = false");
    }
    let (provider, model) = resolve_code_doc_model(state).await?;
    let max_files = setting_i64(state, "kb.code_doc.max_files", 50).await;
    let max_bytes = setting_i64(state, "kb.code_doc.max_file_bytes", 200_000).await as usize;

    let root: String = sqlx::query_scalar(
        "SELECT COALESCE(r.root_path, p.repository_root_path) \
         FROM projects p LEFT JOIN repositories r ON r.project_id = p.id \
         WHERE p.id = $1 LIMIT 1",
    )
    .bind(project_id)
    .fetch_one(&state.db)
    .await
    .context("root progetto non trovata")?;

    let files: Vec<String> = sqlx::query_scalar(
        "SELECT file_path FROM project_code_nodes WHERE project_id = $1 ORDER BY file_path LIMIT $2",
    )
    .bind(project_id)
    .bind(max_files)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let mut ok = 0usize;
    let mut skip = 0usize;
    for rel in files {
        let full = Path::new(&root).join(&rel);
        let content = match std::fs::read_to_string(&full) {
            Ok(c) if c.len() <= max_bytes => c,
            _ => {
                skip += 1;
                continue;
            }
        };
        match generate_code_doc_for_file(state, project_id, &rel, &content, &provider, &model).await {
            Ok(_) => ok += 1,
            Err(e) => {
                tracing::warn!(file = %rel, error = %e, "code_doc: generazione fallita");
                skip += 1;
            }
        }
    }
    tracing::info!(project_id = %project_id, generati = ok, saltati = skip, "code_doc: wiki generata");
    Ok((ok, skip))
}

/// POST /api/projects/:id/knowledge/code-wiki/generate
///
/// Avvia la generazione della code-wiki del progetto. La generazione e' lunga
/// (molte chiamate LLM), quindi gira detached: l'endpoint ritorna subito e le
/// note `code_doc` compaiono man mano (eventi KnowledgeNoteCreated).
pub async fn generate_code_wiki_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    ensure_project_access(&state.db, user_id, project_id).await?;

    let st = state.clone();
    tokio::spawn(async move {
        match generate_code_wiki_for_project(&st, project_id).await {
            Ok((ok, skip)) => {
                tracing::info!(generati = ok, saltati = skip, "code_doc: wiki on-demand completata")
            }
            Err(e) => tracing::warn!(error = %e, "code_doc: wiki on-demand fallita"),
        }
    });

    Ok(Json(json!({ "ok": true, "started": true })))
}
