// Indicizzazione vettoriale del progetto: bootstrap, code files, reindex singolo file.

use super::*;

// ── Helper di chunking ────────────────────────────────────────────────────────

pub(super) fn chunk_text(input: &str, max_chars: usize, max_chunks: usize) -> Vec<String> {
    if max_chars == 0 || max_chunks == 0 {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut current = String::new();

    for paragraph in input.lines() {
        let trimmed = paragraph.trim();
        if trimmed.is_empty() {
            continue;
        }
        if current.len() + trimmed.len() + 1 > max_chars && !current.is_empty() {
            chunks.push(current.trim().to_string());
            current.clear();
            if chunks.len() >= max_chunks {
                break;
            }
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(trimmed);
    }

    if !current.trim().is_empty() && chunks.len() < max_chunks {
        chunks.push(current.trim().to_string());
    }
    chunks
}

// ── Indicizzazione bootstrap ──────────────────────────────────────────────────

/// Documento di indicizzazione: (chiave sorgente, titolo, corpo testuale).
type BootstrapDoc = (String, String, String);

/// Riassume le prime 6 lingue rilevate nella forma "linguaggio (n file)".
/// Estratto da `build_project_summary`.
fn summarize_languages(languages: &[Value]) -> String {
    languages
        .iter()
        .take(6)
        .filter_map(|entry| {
            let language = entry.get("language").and_then(Value::as_str)?;
            let count = entry.get("fileCount").and_then(Value::as_u64).unwrap_or(0);
            Some(format!("{language} ({count})"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Riassume i primi 8 framework/build tool rilevati. Estratto da
/// `build_project_summary`.
fn summarize_frameworks(frameworks: &[String]) -> String {
    if frameworks.is_empty() {
        "nessuno rilevato".to_string()
    } else {
        frameworks
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Costruisce il testo del documento "Project Summary" dai metadati del
/// progetto. Estratto da `index_project_bootstrap_vectors` per contenerne la
/// lunghezza.
fn build_project_summary(
    project_id: Uuid,
    total_files: u32,
    languages: &[Value],
    frameworks: &[String],
    dependencies: &Value,
    git_info: &Value,
) -> String {
    let langs_summary = summarize_languages(languages);
    let frameworks_summary = summarize_frameworks(frameworks);
    let node_deps = dependencies
        .get("node")
        .and_then(|n| n.get("dependencies"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let node_dev_deps = dependencies
        .get("node")
        .and_then(|n| n.get("devDependencies"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let branch = git_info
        .get("branch")
        .and_then(Value::as_str)
        .unwrap_or("n/a");
    let dirty_files = git_info
        .get("dirtyFiles")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    format!(
        "Project bootstrap summary\nProject: {}\nTotal files scanned: {}\nLanguages: {}\nFrameworks/build tools: {}\nNode dependencies: {} (dev: {})\nGit branch: {}\nDirty files: {}\nAnalyzed at: {}",
        project_id,
        total_files,
        if langs_summary.is_empty() { "n/a" } else { &langs_summary },
        frameworks_summary,
        node_deps,
        node_dev_deps,
        branch,
        dirty_files,
        chrono::Utc::now().to_rfc3339()
    )
}

/// Aggiunge a `cmd_lines` i comandi di build/run per progetti .NET/C# se
/// vengono trovati file `.sln` o `.csproj` nella root o nelle sue sottodirectory
/// (escluse quelle di build/deps). Estratto da `index_project_bootstrap_vectors`.
fn append_dotnet_commands(root: &Path, cmd_lines: &mut Vec<String>) {
    let dotnet_dirs: Vec<std::path::PathBuf> = {
        let mut v = vec![root.to_path_buf()];
        if let Ok(rd) = std::fs::read_dir(root) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    let n = p.file_name().and_then(|x| x.to_str()).unwrap_or("");
                    if !matches!(n, "node_modules" | ".git" | "obj" | "bin") {
                        v.push(p);
                    }
                }
            }
        }
        v
    };
    let find_by_ext = |ext: &'static str| -> Option<std::path::PathBuf> {
        dotnet_dirs.iter().find_map(|d| {
            std::fs::read_dir(d).ok()?.flatten().find_map(|e| {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some(ext) {
                    Some(p)
                } else {
                    None
                }
            })
        })
    };
    let sln_file = find_by_ext("sln");
    let csproj_file = find_by_ext("csproj");
    if let Some(sln) = sln_file {
        let rel = sln.strip_prefix(root).unwrap_or(&sln);
        cmd_lines.push(format!(
            "dotnet run --project {} → avvia backend .NET",
            rel.display()
        ));
        cmd_lines.push("dotnet build → compila il progetto .NET".to_string());
        cmd_lines.push("dotnet test → esegui i test .NET".to_string());
    } else if let Some(csproj) = csproj_file {
        let rel = csproj.strip_prefix(root).unwrap_or(&csproj);
        cmd_lines.push(format!(
            "dotnet run --project {} → avvia backend .NET",
            rel.display()
        ));
    }
}

/// Costruisce il documento "Dev Commands" dai comandi npm/cargo/.NET/Python
/// rilevati. Ritorna `None` se non c'e' alcun comando da elencare. Estratto da
/// `index_project_bootstrap_vectors`.
fn build_dev_commands_doc(
    project_id: Uuid,
    root: &Path,
    dependencies: &Value,
) -> Option<BootstrapDoc> {
    let mut cmd_lines: Vec<String> = Vec::new();
    if let Some(scripts) = dependencies
        .get("node")
        .and_then(|n| n.get("scripts"))
        .and_then(|s| s.as_object())
    {
        for (name, val) in scripts.iter().take(12) {
            if let Some(cmd) = val.as_str() {
                cmd_lines.push(format!("npm run {name} → {cmd}"));
            }
        }
    }
    if root.join("Cargo.toml").is_file() {
        cmd_lines.push("cargo build → compila il progetto Rust".to_string());
        cmd_lines.push("cargo run → avvia il progetto Rust".to_string());
        cmd_lines.push("cargo test → esegui i test Rust".to_string());
    }
    append_dotnet_commands(root, &mut cmd_lines);
    // Python
    if root.join("requirements.txt").is_file() || root.join("pyproject.toml").is_file() {
        cmd_lines.push("pip install -r requirements.txt → installa dipendenze".to_string());
    }
    if root.join("manage.py").is_file() {
        cmd_lines.push("python manage.py runserver → avvia server Django".to_string());
    }
    if cmd_lines.is_empty() {
        return None;
    }
    let body = format!(
        "Dev commands per il progetto (Project: {})\n{}",
        project_id,
        cmd_lines.join("\n")
    );
    Some(("dev_commands".to_string(), "Dev Commands".to_string(), body))
}

/// Legge il README (se presente) e appende i suoi chunk a `documents`. Estratto
/// da `index_project_bootstrap_vectors`.
async fn append_readme_docs(root: &Path, documents: &mut Vec<BootstrapDoc>) {
    let readme_path = if root.join("README.md").is_file() {
        Some(root.join("README.md"))
    } else if root.join("readme.md").is_file() {
        Some(root.join("readme.md"))
    } else {
        None
    };
    let Some(path) = readme_path else {
        return;
    };
    if let Ok(content) = fs::read_to_string(&path).await {
        let sanitized = content.trim();
        if !sanitized.is_empty() {
            for (idx, chunk) in chunk_text(sanitized, 1800, 4).into_iter().enumerate() {
                documents.push((
                    format!("readme-{}", idx + 1),
                    format!("README chunk {}", idx + 1),
                    chunk,
                ));
            }
        }
    }
}

/// Estrae la git-history recente (se il progetto e' un repo git) e la appende a
/// `documents` in chunk da 20 commit. Estratto da
/// `index_project_bootstrap_vectors`.
async fn append_git_history_docs(root: &Path, git_info: &Value, documents: &mut Vec<BootstrapDoc>) {
    let is_git_repo = git_info
        .get("isGitRepo")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !is_git_repo {
        return;
    }
    let Ok((stdout, _)) = run_git_command(
        root,
        &[
            "log",
            "--date=short",
            "--pretty=format:%h|%ad|%an|%s",
            "-n",
            "120",
        ],
    )
    .await
    else {
        return;
    };
    let lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    for (chunk_idx, chunk) in lines.chunks(20).take(4).enumerate() {
        let body = format!("Git history chunk {}\n{}", chunk_idx + 1, chunk.join("\n"));
        documents.push((
            format!("history-{}", chunk_idx + 1),
            format!("Git History {}", chunk_idx + 1),
            body,
        ));
    }
}

/// Calcola l'ID deterministico di un punto del vector store: UUID dai primi 16
/// byte di SHA256(project_id || marker || parts unite da ':'). Punto unico
/// dell'hash point-id (regola L), condiviso da indice bootstrap e code index.
/// Lo stream di byte e' identico a quello delle due implementazioni precedenti,
/// quindi gli ID dei punti gia' presenti nel vector store restano invariati.
fn deterministic_point_id(project_id: Uuid, marker: &[u8], parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(project_id.as_bytes());
    hasher.update(marker);
    for (idx, part) in parts.iter().enumerate() {
        if idx > 0 {
            hasher.update(b":");
        }
        hasher.update(part.as_bytes());
    }
    let hash_bytes = hasher.finalize();
    let uuid_bytes: [u8; 16] = hash_bytes[..16].try_into().expect("sha256>=16");
    uuid::Uuid::from_bytes(uuid_bytes).to_string()
}

/// Costruisce il payload JSON di un punto dell'indice bootstrap. Gemello di
/// `code_point_payload`. Estratto da `embed_and_upsert_bootstrap_docs`.
///
/// E' l'UNICO produttore della collection `project_context`, quindi le chiavi
/// che scrive qui (`title`, `text`) sono le sole che un lettore di quella
/// collection possa trovare: `agent_tools::semantic_tools` si prova contro
/// questa funzione invece che contro un payload riscritto a mano (regola O).
pub(crate) fn bootstrap_point_payload(
    project_id: Uuid,
    key: &str,
    title: &str,
    text: &str,
) -> Value {
    json!({
        "project_id": project_id.to_string(),
        "type": "project_bootstrap",
        "source": key,
        "title": title,
        "text": text,
        "active": true,
        "indexed_at": chrono::Utc::now().to_rfc3339(),
    })
}

/// Esito dell'embedding+upsert dei documenti bootstrap.
struct BootstrapUpsertOutcome {
    indexed_points: usize,
    failed_points: usize,
    first_error: Option<String>,
}

/// Embedda e upserta ogni documento bootstrap nel vector store. Estratto da
/// `index_project_bootstrap_vectors` per contenerne complessita e lunghezza.
async fn embed_and_upsert_bootstrap_docs(
    state: &AppState,
    project_id: Uuid,
    documents: &[BootstrapDoc],
    mut first_error: Option<String>,
) -> BootstrapUpsertOutcome {
    let mut indexed_points = 0usize;
    let mut failed_points = 0usize;

    for (key, title, text) in documents.iter() {
        let embedding = match state.orchestrator.embed_text(text).await {
            Ok(vector) => vector,
            Err(error) => {
                failed_points += 1;
                if first_error.is_none() {
                    first_error = Some(format!("embedding fallito per '{title}': {error}"));
                }
                continue;
            }
        };

        let point_id = deterministic_point_id(project_id, b":project_bootstrap:", &[key.as_str()]);
        let payload = bootstrap_point_payload(project_id, key, title, text);

        match vector_memory::upsert_project_context_point(&state.db, &point_id, &embedding, payload)
            .await
        {
            Ok(()) => indexed_points += 1,
            Err(error) => {
                failed_points += 1;
                if first_error.is_none() {
                    first_error = Some(format!("upsert fallito per '{title}': {error}"));
                }
            }
        }
    }

    BootstrapUpsertOutcome {
        indexed_points,
        failed_points,
        first_error,
    }
}

pub async fn index_project_bootstrap_vectors(
    state: &AppState,
    project_id: Uuid,
    root: &Path,
    total_files: u32,
    languages: &[Value],
    frameworks: &[String],
    dependencies: &Value,
    git_info: &Value,
) -> Value {
    let mut documents: Vec<BootstrapDoc> = Vec::new();

    let project_summary = build_project_summary(
        project_id,
        total_files,
        languages,
        frameworks,
        dependencies,
        git_info,
    );
    documents.push((
        "summary".to_string(),
        "Project Summary".to_string(),
        project_summary,
    ));

    if let Some(dev_doc) = build_dev_commands_doc(project_id, root, dependencies) {
        documents.push(dev_doc);
    }

    append_readme_docs(root, &mut documents).await;
    append_git_history_docs(root, git_info, &mut documents).await;

    if documents.is_empty() {
        return json!({
            "status": "skipped",
            "indexedPoints": 0,
            "failedPoints": 0,
            "documents": 0,
            "reason": "Nessun contenuto utile da indicizzare",
        });
    }

    upsert_bootstrap_documents_and_report(state, project_id, &documents).await
}

/// Cancella l'indice bootstrap precedente, embedda e upserta i documenti e
/// costruisce il JSON di esito. L'errore di cleanup e' passato come primo errore
/// gia' noto, cosi' resta visibile in risposta anche se l'upsert va a buon fine.
/// Estratto da `index_project_bootstrap_vectors` per contenerne la lunghezza.
async fn upsert_bootstrap_documents_and_report(
    state: &AppState,
    project_id: Uuid,
    documents: &[BootstrapDoc],
) -> Value {
    let collection = vector_memory::project_context_collection_name(&state.db)
        .await
        .unwrap_or_else(|_| "project_context".to_string());

    let cleanup_error =
        match vector_memory::delete_project_bootstrap_points(&state.db, project_id).await {
            Err(error) => Some(format!("cleanup index precedente: {error}")),
            Ok(()) => None,
        };

    let outcome =
        embed_and_upsert_bootstrap_docs(state, project_id, documents, cleanup_error).await;

    json!({
        "status": index_status(outcome.indexed_points, outcome.failed_points),
        "collection": collection,
        "documents": documents.len(),
        "indexedPoints": outcome.indexed_points,
        "failedPoints": outcome.failed_points,
        "error": outcome.first_error,
        "updatedAt": chrono::Utc::now().to_rfc3339(),
    })
}

// ── Indicizzazione file codice ────────────────────────────────────────────────

// safety: pattern literal valido
static RE_JSX_TEXT_LABEL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r">\s*([A-Za-zÀ-ÿ][^<>{}\n]{3,60})\s*<").unwrap()
});

// safety: pattern literal valido
static RE_UI_PROP_LABEL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r#"(?:title|label|placeholder|aria-label)=["']([^"']{3,60})["']"#).unwrap()
});

// Parametri di chunking condivisi da indicizzazione code files e reindex singolo.
const CODE_MAX_FILE_BYTES: u64 = 200 * 1024;
const CODE_MAX_CHUNKS_PER_FILE: usize = 10;
const CODE_CHUNK_SIZE: usize = 2000;
const CODE_CHUNK_OVERLAP: usize = 200;

// ── Helper condivisi indicizzazione codice (punto unico, regola L) ────────────

/// Estrae fino a 20 label UI (testo JSX + prop title/label/placeholder/aria-label)
/// dai file `tsx`/`jsx`/`vue`. Per gli altri tipi ritorna un vettore vuoto.
/// Punto unico: prima la stessa logica era duplicata in `index_project_code_files`
/// e `reindex_single_file_inner` (l'una con regex statiche, l'altra ricompilate).
fn extract_ui_labels(ext: &str, content: &str) -> Vec<String> {
    if !matches!(ext, "tsx" | "jsx" | "vue") {
        return Vec::new();
    }
    let mut labels: Vec<String> = Vec::new();
    for cap in RE_JSX_TEXT_LABEL.captures_iter(content) {
        let label = cap[1].trim().to_string();
        if !labels.contains(&label) {
            labels.push(label);
        }
        if labels.len() >= 20 {
            break;
        }
    }
    if labels.len() < 20 {
        for cap in RE_UI_PROP_LABEL.captures_iter(content) {
            let label = cap[1].trim().to_string();
            if !labels.contains(&label) {
                labels.push(label);
            }
            if labels.len() >= 20 {
                break;
            }
        }
    }
    labels
}

/// Suddivide il contenuto di un file in chunk testuali con overlap. Il primo
/// chunk e' preceduto dallo `header` (metadati file), i successivi da una
/// intestazione minimale con l'indice del chunk. Punto unico del chunking dei
/// file di codice (regola L), condiviso da indicizzazione e reindex.
fn build_code_chunks(relative_path: &str, header: &str, content: &str) -> Vec<String> {
    if content.len() <= CODE_CHUNK_SIZE {
        return vec![format!("{header}{}", &content[..content.len().min(2000)])];
    }
    let mut result = Vec::new();
    let chars: Vec<char> = content.chars().collect();
    let mut start = 0;
    while start < chars.len() && result.len() < CODE_MAX_CHUNKS_PER_FILE {
        let end = (start + CODE_CHUNK_SIZE).min(chars.len());
        let chunk_content: String = chars[start..end].iter().collect();
        let chunk_idx = result.len();
        let text = if chunk_idx == 0 {
            format!("{header}{chunk_content}")
        } else {
            format!("File: {relative_path}\nChunk: {chunk_idx}\n\n{chunk_content}")
        };
        result.push(text);
        if end >= chars.len() {
            break;
        }
        start = end.saturating_sub(CODE_CHUNK_OVERLAP);
    }
    result
}

/// Calcola l'ID deterministico (UUID da SHA256) di un punto del code index a
/// partire da progetto, path relativo e indice del chunk. Delega a
/// `deterministic_point_id` (punto unico dell'hash point-id, regola L).
fn code_point_id(project_id: Uuid, relative_path: &str, chunk_index: usize) -> String {
    deterministic_point_id(
        project_id,
        b":code:",
        &[relative_path, &chunk_index.to_string()],
    )
}

/// Deriva il path del file relativo alla root del progetto, normalizzato con
/// separatori POSIX; se il file e' fuori dalla root usa il path completo
/// normalizzato. Punto unico (regola L): la stessa derivazione era duplicata in
/// `process_code_file_for_index` e `prepare_reindex`.
fn relative_code_path(root: &Path, file_path: &Path) -> String {
    file_path
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| file_path.to_string_lossy().replace('\\', "/"))
}

/// Estrae le label UI del file e ne costruisce i chunk, anteponendo al primo
/// l'header con i metadati. Ritorna `(label UI, chunk)`. Punto unico (regola L):
/// la stessa sequenza label -> header -> chunk era duplicata in
/// `index_single_code_file` e `prepare_reindex`.
fn build_labeled_code_chunks(
    relative_path: &str,
    ext: &str,
    content: &str,
) -> (Vec<String>, Vec<String>) {
    let ui_labels = extract_ui_labels(ext, content);
    let labels_str = ui_labels.join(", ");
    let header = format!("File: {relative_path}\nTipo: {ext}\nLabels UI: {labels_str}\n\n");
    let chunks = build_code_chunks(relative_path, &header, content);
    (ui_labels, chunks)
}

/// Vero se il file rientra nello scope dell'indice vettoriale: estensione di
/// codice ed entro la soglia di dimensione. Con `skip_generated` esclude anche i
/// file generati (`*.min.*`, `*.d.*`). Punto unico del gate estensione+dimensione
/// (regola L): `collect_code_files_for_index` salta i generati, `prepare_reindex`
/// no — differenza storica preservata dal parametro, non appiattita.
async fn is_code_file_in_scope(path: &Path, skip_generated: bool) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if !CODE_EXTENSIONS.contains(&ext) {
        return false;
    }
    if skip_generated {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem.ends_with(".min") || stem.ends_with(".d") {
            return false;
        }
    }
    if let Ok(meta) = tokio::fs::metadata(path).await {
        if meta.len() > CODE_MAX_FILE_BYTES {
            return false;
        }
    }
    true
}

/// Costruisce il payload JSON di un punto del code index. Punto unico del
/// payload (regola L), condiviso da indicizzazione e reindex.
fn code_point_payload(
    project_id: Uuid,
    relative_path: &str,
    chunk_index: usize,
    ui_labels: &[String],
    ext: &str,
    chunk_text: &str,
) -> Value {
    json!({
        "project_id": project_id.to_string(),
        "type": "code_file",
        "file_path": relative_path,
        "chunk_index": chunk_index,
        "ui_labels": ui_labels,
        "file_ext": ext,
        "text": chunk_text,
        "active": true,
        "indexed_at": chrono::Utc::now().to_rfc3339(),
    })
}

/// Raccoglie ricorsivamente i file di codice sotto `root` (fino a `MAX_FILES`),
/// saltando le directory di build/deps, i file generati (`.min`/`.d`) e quelli
/// oltre la soglia di dimensione. Estratta da `index_project_code_files` per
/// contenerne complessita e lunghezza.
async fn collect_code_files_for_index(root: &Path) -> Vec<std::path::PathBuf> {
    const SKIP_DIRS: &[&str] = &[
        "node_modules",
        ".git",
        "target",
        "obj",
        "bin",
        "dist",
        ".next",
        "__pycache__",
        ".deploy",
    ];
    const MAX_FILES: usize = 500;

    let mut pending_dirs: Vec<std::path::PathBuf> = vec![root.to_path_buf()];
    let mut file_list: Vec<std::path::PathBuf> = Vec::new();

    while let Some(dir) = pending_dirs.pop() {
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(r) => r,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();

            if path.is_dir() {
                let name = entry.file_name();
                if !SKIP_DIRS.contains(&name.to_string_lossy().as_ref()) {
                    pending_dirs.push(path);
                }
                continue;
            }

            if !is_code_file_in_scope(&path, true).await {
                continue;
            }

            file_list.push(path);
            if file_list.len() >= MAX_FILES {
                break;
            }
        }
        if file_list.len() >= MAX_FILES {
            break;
        }
    }
    file_list
}

/// Esito dell'indicizzazione di un singolo file di codice: chunk indicizzati e
/// chunk falliti (con il primo errore incontrato).
#[derive(Default)]
struct CodeFileIndexOutcome {
    chunks_indexed: usize,
    failed: usize,
    first_error: Option<String>,
}

impl CodeFileIndexOutcome {
    /// Contabilizza un chunk fallito, trattenendo solo il primo errore.
    fn record_failure(&mut self, message: String) {
        self.failed += 1;
        if self.first_error.is_none() {
            self.first_error = Some(message);
        }
    }
}

/// Indicizza i chunk di un singolo file: estrae le label UI, costruisce i chunk,
/// li embedda e li upserta nel code index. Estratta da `index_project_code_files`
/// per contenerne complessita e lunghezza.
async fn index_single_code_file(
    state: &AppState,
    project_id: Uuid,
    relative_path: &str,
    ext: &str,
    content: &str,
) -> CodeFileIndexOutcome {
    let mut outcome = CodeFileIndexOutcome::default();
    let (ui_labels, chunks) = build_labeled_code_chunks(relative_path, ext, content);

    for (chunk_index, chunk_text) in chunks.iter().enumerate() {
        let embedding = match state.orchestrator.embed_text(chunk_text).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "code index: embed failed for {relative_path} chunk {chunk_index}: {e}"
                );
                outcome.record_failure(format!("embed fallito per '{relative_path}': {e}"));
                continue;
            }
        };

        let point_id = code_point_id(project_id, relative_path, chunk_index);
        let payload = code_point_payload(
            project_id,
            relative_path,
            chunk_index,
            &ui_labels,
            ext,
            chunk_text,
        );

        match vector_memory::upsert_code_index_point(&state.db, &point_id, &embedding, payload)
            .await
        {
            Ok(()) => outcome.chunks_indexed += 1,
            Err(e) => {
                tracing::warn!(
                    "code index: upsert failed for {relative_path} chunk {chunk_index}: {e}"
                );
                outcome.record_failure(format!("upsert fallito per '{relative_path}': {e}"));
            }
        }
    }
    outcome
}

/// Deriva lo status testuale dell'indicizzazione dai contatori di successo e
/// fallimento. Punto unico dello stato "indexed/partial/error/skipped".
fn index_status(indexed: usize, failed: usize) -> &'static str {
    if indexed > 0 {
        if failed > 0 {
            "partial"
        } else {
            "indexed"
        }
    } else if failed > 0 {
        "error"
    } else {
        "skipped"
    }
}

/// Legge, indicizza e persiste il code-graph di un singolo file durante
/// l'indicizzazione iniziale. Ritorna `None` se il file non e' leggibile (il
/// chiamante lo conta come fallito senza incrementare i file processati).
/// Estratto da `index_project_code_files` per contenerne complessita e lunghezza.
async fn process_code_file_for_index(
    state: &AppState,
    project_id: Uuid,
    root: &Path,
    file_path: &Path,
) -> Option<CodeFileIndexOutcome> {
    let ext = file_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string();
    let relative_path = relative_code_path(root, file_path);

    let content = match tokio::fs::read_to_string(file_path).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("code index: cannot read {relative_path}: {e}");
            return None;
        }
    };

    let outcome = index_single_code_file(state, project_id, &relative_path, &ext, &content).await;

    // Garantisce la scheda KB (wiki_doc kind='code') + le triple del
    // code-graph per ogni file indicizzato anche nell'indicizzazione INIZIALE,
    // non solo nei reindex successivi. Best-effort, idempotente (ON CONFLICT):
    // senza questo i file (specie HTML/JS) comparivano nella KB solo dopo una
    // modifica che innescava reindex_single_file. Punto unico in code_graph.
    let _ = crate::wiki::code_graph::persist_code_graph_for_file(
        &state.db,
        project_id,
        &relative_path,
        &content,
    )
    .await;

    Some(outcome)
}

pub async fn index_project_code_files(state: &AppState, project_id: Uuid, root: &Path) -> Value {
    // Cancella index precedente
    if let Err(e) = vector_memory::delete_code_index_points(&state.db, project_id).await {
        tracing::warn!("code index: cleanup failed: {e}");
    }

    let mut files_processed = 0usize;
    let mut chunks_indexed = 0usize;
    let mut failed = 0usize;
    let mut first_error: Option<String> = None;

    let file_list = collect_code_files_for_index(root).await;

    for file_path in &file_list {
        match process_code_file_for_index(state, project_id, root, file_path).await {
            Some(outcome) => {
                chunks_indexed += outcome.chunks_indexed;
                failed += outcome.failed;
                if first_error.is_none() {
                    first_error = outcome.first_error;
                }
                files_processed += 1;
            }
            None => failed += 1,
        }
    }

    json!({
        "status": index_status(chunks_indexed, failed),
        "files_processed": files_processed,
        "chunks_indexed": chunks_indexed,
        "failed": failed,
        "error": first_error,
        "updated_at": chrono::Utc::now().to_rfc3339(),
    })
}

/// Re-indicizza un singolo file nel code index.
pub async fn reindex_single_file(
    db: &sqlx::PgPool,
    neural: &crate::orchestrator::NeuralCoreClient,
    project_id: Uuid,
    root: &Path,
    file_path: &Path,
) -> anyhow::Result<usize> {
    reindex_single_file_inner(db, neural, project_id, root, file_path, false).await
}

/// Variante di `reindex_single_file` che, con `force=true`, ignora il check
/// hash interno (`file_index_hashes`) e rigenera comunque chunk vettoriali e
/// code-graph triple. Usato dal reindex forzato dell'intero progetto. Con
/// `force=false` il comportamento e' identico a `reindex_single_file`.
/// Materiale pronto per il reindex di un file: path relativo, estensione,
/// contenuto, hash, label UI e chunk gia' costruiti.
struct ReindexPrep {
    relative_path: String,
    ext: String,
    content: String,
    content_hash: String,
    ui_labels: Vec<String>,
    chunks: Vec<String>,
}

/// Vero se il contenuto del file e' identico a quello gia' indicizzato (hash
/// memorizzato in `file_index_hashes`), quindi il reindex e' superfluo. Un
/// errore di lettura equivale a "hash assente" e quindi a contenuto cambiato:
/// comportamento invariato rispetto all'`unwrap_or(None)` originale. Estratto da
/// `prepare_reindex`.
async fn is_content_hash_unchanged(
    db: &sqlx::PgPool,
    project_id: Uuid,
    relative_path: &str,
    content_hash: &str,
) -> bool {
    let stored_hash: Option<String> = sqlx::query_scalar(
        "SELECT content_hash FROM file_index_hashes WHERE project_id = $1 AND file_path = $2",
    )
    .bind(project_id)
    .bind(relative_path)
    .fetch_optional(db)
    .await
    .unwrap_or(None);

    stored_hash.as_deref() == Some(content_hash)
}

/// Prepara il materiale per il reindex di un file: valida estensione e
/// dimensione, legge il contenuto, applica il gate dell'hash (skip se invariato
/// e `force=false`), cancella i chunk precedenti e costruisce i chunk nuovi.
/// Ritorna `Ok(None)` quando il file va saltato. Estratto da
/// `reindex_single_file_inner` per contenerne complessita e lunghezza.
async fn prepare_reindex(
    db: &sqlx::PgPool,
    project_id: Uuid,
    root: &Path,
    file_path: &Path,
    force: bool,
) -> anyhow::Result<Option<ReindexPrep>> {
    if !is_code_file_in_scope(file_path, false).await {
        return Ok(None);
    }
    let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let relative_path = relative_code_path(root, file_path);

    let content = match tokio::fs::read_to_string(file_path).await {
        Ok(c) => c,
        Err(e) => return Err(anyhow::anyhow!("cannot read {relative_path}: {e}")),
    };

    // Calcola hash SHA256 del contenuto e salta se invariato
    let content_hash = sha256_hex(content.as_bytes());
    if !force && is_content_hash_unchanged(db, project_id, &relative_path, &content_hash).await {
        tracing::debug!("reindex_single_file: {relative_path} — hash invariato, skip");
        return Ok(None);
    }

    // Cancella i chunk precedenti di questo file
    vector_memory::delete_code_index_file_points(db, project_id, &relative_path)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("reindex_single_file: cleanup failed for {relative_path}: {e}")
        });

    let (ui_labels, chunks) = build_labeled_code_chunks(&relative_path, ext, &content);

    Ok(Some(ReindexPrep {
        relative_path,
        ext: ext.to_string(),
        content,
        content_hash,
        ui_labels,
        chunks,
    }))
}

pub async fn reindex_single_file_inner(
    db: &sqlx::PgPool,
    neural: &crate::orchestrator::NeuralCoreClient,
    project_id: Uuid,
    root: &Path,
    file_path: &Path,
    force: bool,
) -> anyhow::Result<usize> {
    let Some(prep) = prepare_reindex(db, project_id, root, file_path, force).await? else {
        return Ok(0);
    };

    let indexed = embed_and_upsert_code_chunks(
        db,
        neural,
        project_id,
        &prep.relative_path,
        &prep.ext,
        &prep.ui_labels,
        &prep.chunks,
    )
    .await;

    persist_reindex_side_effects(
        db,
        project_id,
        &prep.relative_path,
        &prep.content,
        &prep.content_hash,
        indexed,
    )
    .await;

    tracing::debug!(
        "reindex_single_file: {} → {indexed} chunks",
        prep.relative_path
    );
    Ok(indexed)
}

/// Embedda (via `NeuralCoreClient`) e upserta i chunk di un file nel code index,
/// ritornando il numero di chunk indicizzati. Estratto da
/// `reindex_single_file_inner` per contenerne complessita e lunghezza.
async fn embed_and_upsert_code_chunks(
    db: &sqlx::PgPool,
    neural: &crate::orchestrator::NeuralCoreClient,
    project_id: Uuid,
    relative_path: &str,
    ext: &str,
    ui_labels: &[String],
    chunks: &[String],
) -> usize {
    let mut indexed = 0usize;
    for (chunk_index, chunk_text) in chunks.iter().enumerate() {
        let embedding = match neural.embed_text("", chunk_text).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "reindex_single_file: embed failed for {relative_path}:{chunk_index}: {e}"
                );
                continue;
            }
        };
        let point_id = code_point_id(project_id, relative_path, chunk_index);
        let payload = code_point_payload(
            project_id,
            relative_path,
            chunk_index,
            ui_labels,
            ext,
            chunk_text,
        );

        if let Ok(()) =
            vector_memory::upsert_code_index_point(db, &point_id, &embedding, payload).await
        {
            indexed += 1;
        }
    }
    indexed
}

/// Effetti collaterali post-indicizzazione del reindex singolo: aggiorna l'hash
/// del file (se qualche chunk e' stato indicizzato), persiste le triple del
/// code-graph e marca stale il doc KB del codice. Best-effort: gli errori sono
/// loggati a WARN, mai propagati. Estratto da `reindex_single_file_inner`.
async fn persist_reindex_side_effects(
    db: &sqlx::PgPool,
    project_id: Uuid,
    relative_path: &str,
    content: &str,
    content_hash: &str,
    indexed: usize,
) {
    // Aggiorna hash nel DB se almeno un chunk e' stato indicizzato
    if indexed > 0 {
        sqlx::query(
            "INSERT INTO file_index_hashes (project_id, file_path, content_hash, indexed_at)
             VALUES ($1, $2, $3, NOW())
             ON CONFLICT (project_id, file_path) DO UPDATE
               SET content_hash = EXCLUDED.content_hash, indexed_at = NOW()",
        )
        .bind(project_id)
        .bind(relative_path)
        .bind(content_hash)
        .execute(db)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("reindex_single_file: failed to store hash for {relative_path}: {e}");
            Default::default()
        });
    }

    // ADR 0017 v2 TODO 5 — persist code-graph triple su wiki_concept_triples.
    // Best-effort: errori loggati a WARN, mai propagati (il reindex vettoriale
    // ha precedenza). Vedi `wiki::code_graph` per la logica regex.
    let _ = crate::wiki::code_graph::persist_code_graph_for_file(
        db,
        project_id,
        relative_path,
        content,
    )
    .await;
    // Segnala al code-docs enricher che il sorgente e' cambiato: marca stale il
    // doc kind=code cosi' il worker rigenera la scheda (best-effort, mig 0331).
    crate::wiki::code_docs_enricher::mark_code_doc_stale_if_changed(
        db,
        project_id,
        relative_path,
        content,
    )
    .await;
}

// ── Helper condivisi handler indice ──────────────────────────────────────────

/// Soglia di dimensione file condivisa dagli handler `get_index_status` e
/// `reindex_stale_files`.
const INDEX_MAX_FILE_BYTES: u64 = 200 * 1024;

/// Calcola l'hash SHA256 (esadecimale) di un buffer di byte. Punto unico del
/// calcolo hash contenuto file (regola L).
fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// Risolve `root_path` e `owner_user_id` del progetto e verifica che il
/// chiamante ne sia il proprietario. Punto unico (regola L) dell'auth+lookup
/// condiviso dagli handler `get_index_status` e `reindex_stale_files`.
async fn resolve_owned_project_root(
    state: &AppState,
    claims: &Claims,
    project_id: Uuid,
) -> Result<String, (StatusCode, Json<Value>)> {
    let row = sqlx::query_as::<_, (String, Option<Uuid>)>(
        "SELECT COALESCE(r.root_path, p.analysis_json->>'rootPath', ''), p.owner_user_id FROM projects p LEFT JOIN repositories r ON r.project_id = p.id WHERE p.id = $1"
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Project not found".to_string()))?;

    let (root_path, owner_id) = row;
    let caller_id = parse_user_id(claims).map_err(|e| {
        api_error(
            StatusCode::UNAUTHORIZED,
            e.1 .0["error"]
                .as_str()
                .unwrap_or("Unauthorized")
                .to_string(),
        )
    })?;
    if owner_id != Some(caller_id) {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Access denied".to_string(),
        ));
    }
    Ok(root_path)
}

/// Carica la mappa `file_path -> content_hash` degli hash indicizzati per il
/// progetto. Punto unico condiviso dagli handler indice.
async fn load_stored_hashes(
    state: &AppState,
    project_id: Uuid,
) -> std::collections::HashMap<String, String> {
    let stored: Vec<(String, String)> = sqlx::query_as(
        "SELECT file_path, content_hash FROM file_index_hashes WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    stored.into_iter().collect()
}

/// Restituisce lo stato dell'indice vettoriale: file modificati vs indicizzati
/// Classificazione di un file rispetto all'indice: aggiornato, stale (indicizzato
/// ma modificato), non indicizzato, oppure da ignorare (oltre soglia/illeggibile).
enum IndexFileState {
    UpToDate,
    Stale(String),
    NotIndexed(String),
    Ignored,
}

/// Classifica un singolo file confrontandone l'hash corrente con quello memorizzato.
/// Estratto da `get_index_status` per contenerne complessita e lunghezza.
fn classify_index_file(
    abs_path: &str,
    root_path: &str,
    stored_map: &std::collections::HashMap<String, String>,
) -> IndexFileState {
    let rel = abs_path
        .strip_prefix(root_path)
        .unwrap_or(abs_path)
        .trim_start_matches(['/', '\\']);
    let rel = rel.replace('\\', "/");

    if let Ok(meta) = std::fs::metadata(abs_path) {
        if meta.len() > INDEX_MAX_FILE_BYTES {
            return IndexFileState::Ignored;
        }
    } else {
        return IndexFileState::Ignored;
    }

    let current_hash = match std::fs::read(abs_path) {
        Ok(bytes) => sha256_hex(&bytes),
        Err(_) => return IndexFileState::Ignored,
    };

    match stored_map.get(&rel) {
        Some(h) if h == &current_hash => IndexFileState::UpToDate,
        Some(_) => IndexFileState::Stale(rel),
        None => IndexFileState::NotIndexed(rel),
    }
}

pub async fn get_index_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let root_path = resolve_owned_project_root(&state, &claims, project_id).await?;
    let stored_map = load_stored_hashes(&state, project_id).await;

    let source_files = collect_source_files(&root_path, CODE_EXTENSIONS);

    let mut stale: Vec<String> = Vec::new();
    let mut up_to_date = 0usize;
    let mut not_indexed = 0usize;

    for abs_path in &source_files {
        match classify_index_file(abs_path, &root_path, &stored_map) {
            IndexFileState::UpToDate => up_to_date += 1,
            IndexFileState::Stale(rel) => stale.push(rel),
            IndexFileState::NotIndexed(rel) => {
                not_indexed += 1;
                stale.push(rel);
            }
            IndexFileState::Ignored => {}
        }
    }

    Ok(Json(json!({
        "stale": stale,
        "staleCount": stale.len(),
        "upToDate": up_to_date,
        "notIndexed": not_indexed,
        "totalFiles": source_files.len(),
    })))
}

/// Query param per il reindex: `?force=true` ignora il check hash e
/// reindicizza TUTTI i file di codice (rigenerando anche i code_doc del
/// grafo-import). Default `false` = comportamento "stale" (solo file cambiati).
#[derive(Debug, Default, Deserialize)]
pub struct ReindexQuery {
    #[serde(default)]
    pub force: bool,
}

/// Re-indicizza i file modificati rispetto all'ultimo hash salvato.
/// Con `?force=true` reindicizza TUTTI i file ignorando l'hash.
/// Esito del reindex di un singolo file candidato in `reindex_stale_files`.
enum StaleOutcome {
    Reindexed,
    Skipped,
    Ignored,
}

/// Valuta e (se necessario) reindicizza un singolo file candidato: salta i file
/// oltre soglia/illeggibili, quelli con hash invariato (se non `force`), e
/// delega il resto a `reindex_single_file_inner`. Estratto da
/// `reindex_stale_files` per contenerne complessita e lunghezza.
async fn reindex_one_stale_file(
    state: &AppState,
    project_id: Uuid,
    root_path_obj: &Path,
    abs_path_str: &str,
    stored_map: &std::collections::HashMap<String, String>,
    force: bool,
) -> StaleOutcome {
    let abs_path = std::path::Path::new(abs_path_str);
    if let Ok(meta) = std::fs::metadata(abs_path) {
        if meta.len() > INDEX_MAX_FILE_BYTES {
            return StaleOutcome::Ignored;
        }
    } else {
        return StaleOutcome::Ignored;
    }

    let rel = abs_path
        .strip_prefix(root_path_obj)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| abs_path_str.to_string());

    let current_hash = match std::fs::read(abs_path) {
        Ok(bytes) => sha256_hex(&bytes),
        Err(_) => return StaleOutcome::Ignored,
    };

    if !force
        && stored_map
            .get(&rel)
            .map(|h| h == &current_hash)
            .unwrap_or(false)
    {
        return StaleOutcome::Skipped;
    }

    match reindex_single_file_inner(
        &state.db,
        &state.orchestrator.neural,
        project_id,
        root_path_obj,
        abs_path,
        force,
    )
    .await
    {
        Ok(n) if n > 0 => StaleOutcome::Reindexed,
        _ => StaleOutcome::Ignored,
    }
}

pub async fn reindex_stale_files(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
    Query(q): Query<ReindexQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let root_path = resolve_owned_project_root(&state, &claims, project_id).await?;
    let stored_map = load_stored_hashes(&state, project_id).await;

    let source_files = collect_source_files(&root_path, CODE_EXTENSIONS);
    let root_path_obj = std::path::Path::new(&root_path);

    let mut reindexed = 0usize;
    let mut skipped = 0usize;

    for abs_path_str in &source_files {
        match reindex_one_stale_file(
            &state,
            project_id,
            root_path_obj,
            abs_path_str,
            &stored_map,
            q.force,
        )
        .await
        {
            StaleOutcome::Reindexed => reindexed += 1,
            StaleOutcome::Skipped => skipped += 1,
            StaleOutcome::Ignored => {}
        }
    }

    Ok(Json(json!({
        "reindexed": reindexed,
        "skipped": skipped,
        "total": source_files.len(),
        "forced": q.force,
    })))
}

// ── Trigger automatico indicizzazione ────────────────────────────────────────

/// Avvia l'indicizzazione semantica del progetto in background se:
/// - embedder e Qdrant sono operativi (`dependency_status`)
/// - nessuna indicizzazione e' gia' in corso per questo progetto (`indexing_projects`)
/// - la tabella `file_index_hashes` non ha ancora righe per questo progetto
///
/// Chiamata da `update_user_active_project` ogni volta che un utente switcha
/// progetto o invia il primo messaggio di una sessione.
/// Recupera dalla DB il `root_path` del progetto (repositories.root_path, con
/// fallback su analysis_json e repository_root_path) e verifica che esista su
/// disco. Ritorna `None` (loggando) se manca o non esiste. Estratto da
/// `spawn_code_index_if_needed`.
async fn resolve_existing_project_root(
    state: &AppState,
    project_id: Uuid,
) -> Option<(std::path::PathBuf, String)> {
    let root_opt: Option<(String,)> = sqlx::query_as(
        "SELECT COALESCE(r.root_path, p.analysis_json->>'rootPath', p.repository_root_path, '') \
         FROM projects p \
         LEFT JOIN repositories r ON r.project_id = p.id \
         WHERE p.id = $1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    let root_str = match root_opt {
        Some((r,)) if !r.is_empty() => r,
        _ => {
            tracing::warn!("spawn_code_index_if_needed: project={project_id} senza root, skip");
            return None;
        }
    };

    let root = std::path::PathBuf::from(&root_str);
    if !root.exists() {
        tracing::warn!(
            "spawn_code_index_if_needed: project={project_id} root={root_str} non esiste, skip"
        );
        return None;
    }
    Some((root, root_str))
}

pub async fn spawn_code_index_if_needed(state: &AppState, project_id: Uuid) {
    use std::sync::atomic::Ordering;

    // 1. Infrastruttura pronta?
    let dep = &state.dependency_status;
    if !dep.qdrant.load(Ordering::Relaxed) || !dep.embedder.load(Ordering::Relaxed) {
        tracing::debug!(
            "spawn_code_index_if_needed: skip project={project_id} (qdrant/embedder non pronti)"
        );
        return;
    }

    // 2. Recupera root del progetto (serve sia per indicizzare che per il watcher).
    // Prova prima repositories.root_path (source of truth per i clone locali),
    // poi projects.analysis_json->>'rootPath' come fallback.
    let Some((root, root_str)) = resolve_existing_project_root(state, project_id).await else {
        return;
    };

    // 3. Indicizzazione gia' in corso?
    if state.indexing_projects.contains(&project_id) {
        tracing::debug!("spawn_code_index_if_needed: skip project={project_id} (gia' in corso)");
        // Il watcher verra' avviato al termine del task di indicizzazione gia' in esecuzione.
        return;
    }

    // 4. Gia' indicizzato? → avvia solo il file watcher (idempotente).
    if vector_memory::has_code_index(&state.db, project_id).await {
        tracing::debug!("spawn_code_index_if_needed: skip indicizzazione project={project_id} (gia' indicizzato), avvio watcher");
        crate::projects::file_watcher::spawn_file_watcher(state, project_id, root);
        return;
    }

    // 5. Prima indicizzazione: lancia in background, poi avvia il watcher.
    state.indexing_projects.insert(project_id);
    tracing::info!(
        "spawn_code_index_if_needed: avvio indicizzazione project={project_id} root={root_str}"
    );
    spawn_initial_index_task(state, project_id, root);
}

/// Lancia in background la prima indicizzazione del codice del progetto: al
/// termine libera il lock `indexing_projects` e avvia il file watcher. Il
/// chiamante ha gia' inserito il lock e verificato le precondizioni. Estratto da
/// `spawn_code_index_if_needed`.
fn spawn_initial_index_task(state: &AppState, project_id: Uuid, root: std::path::PathBuf) {
    let state_bg = state.clone();
    tokio::spawn(async move {
        let result = index_project_code_files(&state_bg, project_id, &root).await;
        state_bg.indexing_projects.remove(&project_id);
        tracing::info!(
            "spawn_code_index_if_needed: completata project={project_id} status={}",
            result.get("status").and_then(|v| v.as_str()).unwrap_or("?")
        );
        // Avvia il file watcher ora che l'indice iniziale e' pronto.
        crate::projects::file_watcher::spawn_file_watcher(&state_bg, project_id, root);
    });
}
