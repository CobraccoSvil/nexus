//! FunctionalSpecAgent — estrae specifiche funzionali dai messaggi chat
//! utente di un progetto E dai file del repository (Markdown + sorgenti
//! chiave) via LLM, materializzandole come note `kind='functional'`.
//!
//! Pipeline (extract_functional_specs_for_project):
//!   1. Se `include_files=true`: scansiona ricorsivamente la `repository_root_path`
//!      del progetto e raccoglie file rilevanti (.md, README, sorgenti in
//!      routes/handlers/controllers/models/schema/migrations). Per ciascuno
//!      chiama LLM con `build_file_extraction_prompt`. Skippa target/, node_modules/,
//!      .git/, .nexus/, dist/, build/.
//!   2. Carica chat_messages user del progetto >= 50 char non boilerplate.
//!      Per ogni messaggio chiama LLM con `build_chat_extraction_prompt`.
//!   3. L'LLM ritorna un array JSON di specs (vuoto se non rilevate).
//!   4. Per ogni spec: UPSERT in `project_knowledge_notes` con kind='functional'
//!      via `apply_project_note` (idempotente su project_id+kind+title).
//!   5. Genera embedding + upsert Qdrant per ricerca semantica.
//!
//! L'agente puo' essere triggherato:
//!   - Manualmente via endpoint `POST /api/projects/:id/knowledge/extract-functional`
//!     con body `{"limit": N, "include_files": bool}`
//!   - Periodicamente da worker (futuro)

use crate::knowledge::generators::{apply_project_note, GeneratedProjectNote};
use crate::AppState;
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use sqlx::Row;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Singola specifica funzionale estratta dall'LLM.
#[derive(Debug, Deserialize, Clone)]
pub struct ExtractedSpec {
    pub title: String,
    pub body_md: String,
    /// feature | requirement | user_story | decision | domain | constraint | non_functional
    pub intent: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub file_paths: Vec<String>,
}

/// Risultato del processing.
#[derive(Debug, Default)]
pub struct ExtractStats {
    pub messages_scanned: usize,
    pub messages_skipped_short: usize,
    pub messages_with_specs: usize,
    pub files_scanned: usize,
    pub files_skipped_short: usize,
    pub files_with_specs: usize,
    pub specs_extracted: usize,
    pub specs_applied: usize,
    pub llm_errors: usize,
}

/// Filtra messaggi user che sono *boilerplate* (non contengono specifiche
/// estraibili): conferme brevi, ringraziamenti, comandi di controllo.
fn is_boilerplate(content: &str) -> bool {
    let lower = content.trim().to_lowercase();
    if lower.len() < 50 {
        return true;
    }
    let boilerplate_starters = [
        "ok", "okay", "perfetto", "perfect", "grazie", "thanks", "continua", "continue",
        "riprendi", "resume", "vai", "go", "procedi", "proceed", "si", "yes", "no", "stop",
        "annulla",
    ];
    let first_word = lower.split_whitespace().next().unwrap_or("");
    boilerplate_starters.contains(&first_word) && lower.len() < 150
}

/// Costruisce il prompt LLM per estrarre specifiche funzionali da un messaggio chat.
fn build_chat_extraction_prompt(project_name: &str, message: &str) -> String {
    format!(
        r#"Sei un analista funzionale che estrae specifiche concrete dai messaggi utente di un progetto software.

Progetto: "{project_name}"

Messaggio utente:
\"\"\"
{message}
\"\"\"

Analizza il messaggio e identifica le SPECIFICHE FUNZIONALI presenti. Una specifica e' una di queste:
- **feature**: nuova funzionalita' richiesta (es. "voglio un endpoint per esportare CSV")
- **requirement**: requisito tecnico/business (es. "tutti gli endpoint devono richiedere auth")
- **user_story**: storia utente (es. "come admin voglio bannare utenti")
- **decision**: decisione di design/prodotto (es. "usiamo PostgreSQL invece di MySQL")
- **domain**: concetto di dominio (es. "un Task ha titolo, priorita', stato")
- **constraint**: vincolo (es. "il sistema deve girare su Linux")
- **non_functional**: requisito non funzionale (performance, sicurezza, accessibilita')

Regole:
1. Se il messaggio e' una semplice domanda/comando senza specifica funzionale (es. "che ore sono?", "esegui i test"), ritorna `[]`.
2. Estrai SOLO cio' che e' esplicitamente menzionato — non inventare nulla.
3. Titolo conciso (max 100 char), body markdown ricco con dettagli + razionale dal messaggio.
4. Tags: 2-5 parole chiave specifiche al dominio (no generiche tipo "feature" o "code").
5. file_paths: array di path file menzionati nel messaggio (assoluti o relativi).
6. Un messaggio puo' contenere 0, 1 o piu' specifiche.

Rispondi SOLO con un array JSON valido (parsabile), NESSUN testo extra prima o dopo. Esempio:

[
  {{
    "title": "Endpoint export CSV per tabella tasks",
    "body_md": "L'utente vuole un endpoint REST che esporti tutti i task in formato CSV con colonne id, titolo, priorita, stato.\n\n**Motivazione**: serve per analytics su Excel.",
    "intent": "feature",
    "tags": ["export", "csv", "tasks", "rest-api"],
    "file_paths": ["backend/src/routes/tasks.rs"]
  }}
]

Se nessuna specifica e' presente, rispondi: []"#
    )
}

/// Costruisce il prompt LLM per estrarre specifiche da un file (sorgente o .md).
fn build_file_extraction_prompt(project_name: &str, file_path: &str, content: &str) -> String {
    format!(
        r#"Sei un analista funzionale. Stai analizzando un file di un progetto software per estrarne le SPECIFICHE FUNZIONALI (esplicite o implicite).

Progetto: "{project_name}"
File: `{file_path}`

Contenuto del file (troncato a 8000 caratteri se necessario):
\"\"\"
{content}
\"\"\"

Analizza il file e identifica le SPECIFICHE FUNZIONALI presenti. Tipi consentiti:
- **feature**: funzionalita' fornita (es. "endpoint REST per X", "componente UI Y", "comando CLI Z")
- **requirement**: requisito tecnico/business (es. "tutti gli endpoint devono autenticare")
- **user_story**: storia utente che il file/codice implementa
- **decision**: decisione di design implicita (es. "uso del pattern Repository", "scelta libreria X")
- **domain**: concetto/entita' di dominio modellato dal file (es. "Task ha titolo, priorita', stato")
- **constraint**: vincolo (es. "ports 4000-4099 riservate", "max 100 record per pagina")
- **non_functional**: requisito non funzionale (performance, sicurezza, scalabilita')

Regole rigide:
1. Se il file e' puramente strutturale (config Cargo/package.json senza descrizioni, types vuoti, lockfile), ritorna `[]`.
2. Estrai SOLO cio' che e' DEDUCIBILE dal contenuto. MAI inventare requisiti non presenti.
3. Per file .md (README, docs, ADR): estrai feature/requirement documentati nel testo.
4. Per file sorgente: deduci le feature implementate da signature funzioni pubbliche, nomi endpoint/route, schema DB, comandi CLI, struct di dominio.
5. Titolo conciso (max 100 char). Body markdown con dettagli rilevanti.
6. Tags: 2-5 parole chiave specifiche al dominio (no generiche tipo "code" o "file").
7. file_paths: includi SEMPRE `{file_path}` come primo elemento dell'array.
8. Un file puo' generare 0, 1, o piu' specifiche distinte.

Rispondi SOLO con un array JSON valido (parsabile), NESSUN testo extra. Schema:

[
  {{
    "title": "...",
    "body_md": "...",
    "intent": "feature|requirement|user_story|decision|domain|constraint|non_functional",
    "tags": ["..."],
    "file_paths": ["{file_path}"]
  }}
]

Se nessuna specifica funzionale e' presente, rispondi: []"#
    )
}

/// Risolve il modello LLM da usare per l'extractor via routing matrix.
async fn resolve_llm(state: &AppState) -> Option<(String, String)> {
    let matrix = state
        .orchestrator
        .routing_matrix
        .current_async()
        .await
        .ok()?;
    matrix.purpose_model("functional_spec_extractor")
}

/// Parsa il JSON ritornato dall'LLM in lista di ExtractedSpec.
/// Robusto a markdown code fences e testo extra (estrae il primo array JSON).
fn parse_specs(raw: &str) -> Vec<ExtractedSpec> {
    let trimmed = raw.trim();
    let start = trimmed.find('[');
    let end = trimmed.rfind(']');
    let Some((s, e)) = start.zip(end) else {
        return Vec::new();
    };
    if e <= s {
        return Vec::new();
    }
    let json_str = &trimmed[s..=e];
    serde_json::from_str::<Vec<ExtractedSpec>>(json_str).unwrap_or_default()
}

/// Decide se un file è rilevante per l'estrazione.
fn is_relevant_file(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_lowercase();

    // Tutti i Markdown
    if ext == "md" || ext == "mdx" {
        return true;
    }
    // SQL migrations sono utili (descrivono schema)
    if ext == "sql" {
        return true;
    }

    // Filename keywords
    if file_name.contains("readme")
        || file_name.contains("changelog")
        || file_name.contains("architecture")
        || file_name.contains("design")
        || file_name.contains("spec")
        || file_name.contains("requirements")
        || file_name.contains("prd")
        || file_name.contains("roadmap")
        || file_name.contains("contributing")
    {
        return true;
    }

    // Sorgenti chiave per estensione
    let source_exts = ["rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "kt"];
    if source_exts.contains(&ext.as_str()) {
        // path keywords
        let path_str = path.to_string_lossy().to_lowercase();
        if path_str.contains("/routes/")
            || path_str.contains("/handlers/")
            || path_str.contains("/controllers/")
            || path_str.contains("/models/")
            || path_str.contains("/api/")
            || path_str.contains("/schema/")
            || path_str.contains("/migrations/")
            || path_str.contains("/services/")
            || path_str.contains("/agents/")
            || path_str.contains("/cli/")
            || path_str.contains("/commands/")
            || path_str.contains("/handlers")
            || path_str.contains("/endpoints/")
            || file_name == "main.rs"
            || file_name == "lib.rs"
            || file_name == "index.ts"
            || file_name == "index.tsx"
            || file_name == "app.py"
            || file_name == "main.py"
        {
            return true;
        }
    }
    false
}

/// Directory da escludere durante la scansione (rumore + leak rischio).
fn is_excluded_dir(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "target"
            | "node_modules"
            | ".git"
            | ".nexus"
            | "dist"
            | "build"
            | "__pycache__"
            | ".next"
            | ".turbo"
            | "venv"
            | ".venv"
            | "vendor"
            | "out"
            | "coverage"
            | ".cargo"
            | ".pytest_cache"
            | ".mypy_cache"
            | ".ruff_cache"
            | ".idea"
            | ".vscode"
    )
}

/// Scansione BFS della repo, ritorna fino a `max_files` file rilevanti.
/// Ordinamento: .md prima, poi sorgenti.
fn collect_relevant_files(root: &Path, max_files: usize) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    let mut visited = 0usize;
    const MAX_DIRS: usize = 5000;

    while let Some(dir) = stack.pop() {
        if visited >= MAX_DIRS {
            tracing::warn!("collect_relevant_files: hit MAX_DIRS, stop scan");
            break;
        }
        visited += 1;

        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            // Skip nascosti (eccetto .nexus-vault che NON e' una nostra repo ma docs)
            if name.starts_with('.') && name != ".nexus-vault" {
                continue;
            }
            if path.is_dir() {
                if is_excluded_dir(&name) {
                    continue;
                }
                stack.push(path);
            } else if path.is_file() && is_relevant_file(&path) {
                files.push(path);
                if files.len() >= max_files {
                    break;
                }
            }
        }
        if files.len() >= max_files {
            break;
        }
    }

    // Markdown prima (priorità: README, docs, ADR)
    files.sort_by_key(|p| {
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();
        let is_md = ext == "md" || ext == "mdx";
        let is_readme = p
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_lowercase().contains("readme"))
            .unwrap_or(false);
        match (is_readme, is_md) {
            (true, _) => 0,
            (false, true) => 1,
            _ => 2,
        }
    });

    Ok(files)
}

/// Estrae il testo dalla response JSON di `generate_completion`.
fn extract_text_from_response(raw: &Value) -> String {
    raw.get("text")
        .or_else(|| raw.get("content"))
        .or_else(|| raw.get("completion"))
        .or_else(|| raw.get("output"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Esegue l'estrazione su chat_messages user del progetto.
async fn extract_from_chat_messages(
    state: &AppState,
    project_id: Uuid,
    project_name: &str,
    provider: &str,
    model: &str,
    limit: i64,
    stats: &mut ExtractStats,
) -> Result<()> {
    let rows = sqlx::query(
        r#"
        SELECT cm.id, cm.content
        FROM chat_messages cm
        JOIN chat_sessions cs ON cs.id = cm.session_id
        WHERE cs.project_id = $1
          AND cm.role = 'user'
          AND length(cm.content) >= 50
        ORDER BY cm.created_at DESC
        LIMIT $2
        "#,
    )
    .bind(project_id)
    .bind(limit)
    .fetch_all(&state.db)
    .await
    .context("query chat_messages")?;

    stats.messages_scanned = rows.len();

    for row in rows {
        let content: String = row.try_get("content").unwrap_or_default();
        if is_boilerplate(&content) {
            stats.messages_skipped_short += 1;
            continue;
        }

        let prompt = build_chat_extraction_prompt(project_name, &content);
        let raw_result = match state
            .orchestrator
            .neural
            .generate_completion(provider, model, &prompt)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                stats.llm_errors += 1;
                tracing::debug!(error = %e, "extract_functional chat: LLM fallita");
                continue;
            }
        };
        let raw_text = extract_text_from_response(&raw_result);
        let specs = parse_specs(&raw_text);
        if specs.is_empty() {
            continue;
        }
        stats.messages_with_specs += 1;
        stats.specs_extracted += specs.len();

        for spec in specs {
            let note = GeneratedProjectNote {
                kind: "functional".to_string(),
                title: spec.title,
                body_md: spec.body_md,
                intent: Some(spec.intent),
                tags: spec.tags,
                file_paths: spec.file_paths,
            };
            if apply_project_note(state, project_id, &note).await.is_ok() {
                stats.specs_applied += 1;
            }
        }
    }
    Ok(())
}

/// Esegue l'estrazione sui file rilevanti del repository.
async fn extract_from_repo_files(
    state: &AppState,
    project_id: Uuid,
    project_name: &str,
    repo_root: &str,
    provider: &str,
    model: &str,
    max_files: usize,
    stats: &mut ExtractStats,
) -> Result<()> {
    let root_path = Path::new(repo_root);
    if !root_path.is_dir() {
        anyhow::bail!("Repository root non valido o inaccessibile: {repo_root}");
    }

    let files = collect_relevant_files(root_path, max_files)?;
    stats.files_scanned = files.len();
    tracing::info!(
        project_id = %project_id,
        files = files.len(),
        "extract_functional: file rilevanti raccolti"
    );

    for file_path in files {
        let rel_path = file_path
            .strip_prefix(root_path)
            .unwrap_or(&file_path)
            .to_string_lossy()
            .replace('\\', "/");

        let raw = match tokio::fs::read_to_string(&file_path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(file = %rel_path, error = %e, "read file fallito, skip");
                continue;
            }
        };
        let trimmed = raw.trim();
        if trimmed.len() < 100 {
            stats.files_skipped_short += 1;
            continue;
        }
        let truncated = if trimmed.len() > 8000 {
            &trimmed[..8000]
        } else {
            trimmed
        };

        let prompt = build_file_extraction_prompt(project_name, &rel_path, truncated);
        let raw_result = match state
            .orchestrator
            .neural
            .generate_completion(provider, model, &prompt)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                stats.llm_errors += 1;
                tracing::debug!(file = %rel_path, error = %e, "extract_functional file: LLM fallita");
                continue;
            }
        };
        let raw_text = extract_text_from_response(&raw_result);
        let specs = parse_specs(&raw_text);
        if specs.is_empty() {
            continue;
        }
        stats.files_with_specs += 1;
        stats.specs_extracted += specs.len();

        for spec in specs {
            // Garantisci che il path del file sia incluso in file_paths
            let mut file_paths = spec.file_paths.clone();
            if !file_paths.iter().any(|p| p == &rel_path) {
                file_paths.insert(0, rel_path.clone());
            }
            let note = GeneratedProjectNote {
                kind: "functional".to_string(),
                title: spec.title,
                body_md: spec.body_md,
                intent: Some(spec.intent),
                tags: spec.tags,
                file_paths,
            };
            if apply_project_note(state, project_id, &note).await.is_ok() {
                stats.specs_applied += 1;
            }
        }
    }
    Ok(())
}

/// Esegue l'estrazione su chat_messages e (opzionalmente) sui file del repo.
pub async fn extract_functional_specs_for_project(
    state: &AppState,
    project_id: Uuid,
    chat_limit: Option<i64>,
    include_files: bool,
    files_limit: Option<usize>,
) -> Result<ExtractStats> {
    let chat_limit = chat_limit.unwrap_or(50).clamp(1, 500);
    let files_limit = files_limit.unwrap_or(80).clamp(1, 300);

    let project_name: String = sqlx::query_scalar("SELECT name FROM projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "(unknown)".to_string());

    let Some((provider, model)) = resolve_llm(state).await else {
        anyhow::bail!(
            "routing matrix non disponibile o purpose 'functional_spec_extractor' non configurato"
        );
    };

    let mut stats = ExtractStats::default();

    if include_files {
        let repo_root: Option<String> =
            sqlx::query_scalar("SELECT repository_root_path FROM projects WHERE id = $1")
                .bind(project_id)
                .fetch_optional(&state.db)
                .await
                .ok()
                .flatten();

        if let Some(root) = repo_root {
            if let Err(e) = extract_from_repo_files(
                state,
                project_id,
                &project_name,
                &root,
                &provider,
                &model,
                files_limit,
                &mut stats,
            )
            .await
            {
                tracing::warn!(error = %e, "extract_functional: scansione file fallita");
            }
        } else {
            tracing::info!(
                project_id = %project_id,
                "extract_functional: nessun repository_root_path, skip scansione file"
            );
        }
    }

    extract_from_chat_messages(
        state,
        project_id,
        &project_name,
        &provider,
        &model,
        chat_limit,
        &mut stats,
    )
    .await?;

    tracing::info!(
        project_id = %project_id,
        msg_scanned = stats.messages_scanned,
        msg_with_specs = stats.messages_with_specs,
        files_scanned = stats.files_scanned,
        files_with_specs = stats.files_with_specs,
        specs_extracted = stats.specs_extracted,
        specs_applied = stats.specs_applied,
        llm_errors = stats.llm_errors,
        "extract_functional_specs_for_project: completato"
    );

    Ok(stats)
}
