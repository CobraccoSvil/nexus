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
    let mut documents: Vec<(String, String, String)> = Vec::new();

    let langs_summary = languages
        .iter()
        .take(6)
        .filter_map(|entry| {
            let language = entry.get("language").and_then(Value::as_str)?;
            let count = entry.get("fileCount").and_then(Value::as_u64).unwrap_or(0);
            Some(format!("{language} ({count})"))
        })
        .collect::<Vec<_>>()
        .join(", ");
    let frameworks_summary = if frameworks.is_empty() {
        "nessuno rilevato".to_string()
    } else {
        frameworks
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
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
    let project_summary = format!(
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
    );
    documents.push((
        "summary".to_string(),
        "Project Summary".to_string(),
        project_summary,
    ));

    // Documento comandi di sviluppo
    {
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
        // .NET/C#
        {
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
            let sln_file = dotnet_dirs.iter().find_map(|d| {
                std::fs::read_dir(d).ok()?.flatten().find_map(|e| {
                    let p = e.path();
                    if p.extension().and_then(|x| x.to_str()) == Some("sln") {
                        Some(p)
                    } else {
                        None
                    }
                })
            });
            let csproj_file = dotnet_dirs.iter().find_map(|d| {
                std::fs::read_dir(d).ok()?.flatten().find_map(|e| {
                    let p = e.path();
                    if p.extension().and_then(|x| x.to_str()) == Some("csproj") {
                        Some(p)
                    } else {
                        None
                    }
                })
            });
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
        // Python
        if root.join("requirements.txt").is_file() || root.join("pyproject.toml").is_file() {
            cmd_lines.push("pip install -r requirements.txt → installa dipendenze".to_string());
        }
        if root.join("manage.py").is_file() {
            cmd_lines.push("python manage.py runserver → avvia server Django".to_string());
        }
        if !cmd_lines.is_empty() {
            let body = format!(
                "Dev commands per il progetto (Project: {})\n{}",
                project_id,
                cmd_lines.join("\n")
            );
            documents.push(("dev_commands".to_string(), "Dev Commands".to_string(), body));
        }
    }

    let readme_path = if root.join("README.md").is_file() {
        Some(root.join("README.md"))
    } else if root.join("readme.md").is_file() {
        Some(root.join("readme.md"))
    } else {
        None
    };
    if let Some(path) = readme_path {
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

    if git_info
        .get("isGitRepo")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        if let Ok((stdout, _)) = run_git_command(
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
        {
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
    }

    if documents.is_empty() {
        return json!({
            "status": "skipped",
            "indexedPoints": 0,
            "failedPoints": 0,
            "documents": 0,
            "reason": "Nessun contenuto utile da indicizzare",
        });
    }

    let collection = vector_memory::project_context_collection_name(&state.db)
        .await
        .unwrap_or_else(|_| "project_context".to_string());

    let mut first_error: Option<String> = None;
    let mut indexed_points = 0usize;
    let mut failed_points = 0usize;

    if let Err(error) = vector_memory::delete_project_bootstrap_points(&state.db, project_id).await
    {
        first_error = Some(format!("cleanup index precedente: {error}"));
    }

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

        let mut point_hasher = Sha256::new();
        point_hasher.update(project_id.as_bytes());
        point_hasher.update(b":project_bootstrap:");
        point_hasher.update(key.as_bytes());
        let ph_bytes = point_hasher.finalize();
        let ph_uuid: [u8; 16] = ph_bytes[..16].try_into().expect("sha256>=16");
        let point_id = uuid::Uuid::from_bytes(ph_uuid).to_string();
        let payload = json!({
            "project_id": project_id.to_string(),
            "type": "project_bootstrap",
            "source": key,
            "title": title,
            "text": text,
            "active": true,
            "indexed_at": chrono::Utc::now().to_rfc3339(),
        });

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

    let status = if indexed_points > 0 {
        if failed_points > 0 {
            "partial"
        } else {
            "indexed"
        }
    } else if failed_points > 0 {
        "error"
    } else {
        "skipped"
    };

    json!({
        "status": status,
        "collection": collection,
        "documents": documents.len(),
        "indexedPoints": indexed_points,
        "failedPoints": failed_points,
        "error": first_error,
        "updatedAt": chrono::Utc::now().to_rfc3339(),
    })
}

// ── Indicizzazione file codice ────────────────────────────────────────────────

pub async fn index_project_code_files(state: &AppState, project_id: Uuid, root: &Path) -> Value {
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
    const CODE_EXTENSIONS: &[&str] = &["tsx", "jsx", "ts", "js", "rs", "py", "cs", "go", "vue"];
    const MAX_FILE_BYTES: u64 = 200 * 1024;
    const MAX_FILES: usize = 500;
    const MAX_CHUNKS_PER_FILE: usize = 10;
    const CHUNK_SIZE: usize = 2000;
    const CHUNK_OVERLAP: usize = 200;

    // Cancella index precedente
    if let Err(e) = vector_memory::delete_code_index_points(&state.db, project_id).await {
        tracing::warn!("code index: cleanup failed: {e}");
    }

    let mut files_processed = 0usize;
    let mut chunks_indexed = 0usize;
    let mut failed = 0usize;
    let mut first_error: Option<String> = None;

    // Raccolta file via walk ricorsiva
    let mut pending_dirs: Vec<std::path::PathBuf> = vec![root.to_path_buf()];
    let mut file_list: Vec<std::path::PathBuf> = Vec::new();

    while let Some(dir) = pending_dirs.pop() {
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(r) => r,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if path.is_dir() {
                if !SKIP_DIRS.contains(&name_str.as_ref()) {
                    pending_dirs.push(path);
                }
                continue;
            }

            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !CODE_EXTENSIONS.contains(&ext) {
                continue;
            }

            // Salta file generati
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if stem.ends_with(".min") || stem.ends_with(".d") {
                continue;
            }

            if let Ok(meta) = tokio::fs::metadata(&path).await {
                if meta.len() > MAX_FILE_BYTES {
                    continue;
                }
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

    for file_path in &file_list {
        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();
        let relative_path = file_path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| file_path.to_string_lossy().replace('\\', "/"));

        let content = match tokio::fs::read_to_string(file_path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("code index: cannot read {relative_path}: {e}");
                failed += 1;
                continue;
            }
        };

        // Estrai UI labels per tsx/jsx/vue
        let ui_labels: Vec<String> = if matches!(ext.as_str(), "tsx" | "jsx" | "vue") {
            let mut labels: Vec<String> = Vec::new();
            let re_jsx = regex::Regex::new(r">\s*([A-Za-zÀ-ÿ][^<>{}\n]{3,60})\s*<").unwrap();
            for cap in re_jsx.captures_iter(&content) {
                let label = cap[1].trim().to_string();
                if !labels.contains(&label) {
                    labels.push(label.clone());
                }
                if labels.len() >= 20 {
                    break;
                }
            }
            if labels.len() < 20 {
                let re_props = regex::Regex::new(
                    r#"(?:title|label|placeholder|aria-label)=["']([^"']{3,60})["']"#,
                )
                .unwrap();
                for cap in re_props.captures_iter(&content) {
                    let label = cap[1].trim().to_string();
                    if !labels.contains(&label) {
                        labels.push(label.clone());
                    }
                    if labels.len() >= 20 {
                        break;
                    }
                }
            }
            labels
        } else {
            Vec::new()
        };

        let labels_str = ui_labels.join(", ");
        let header = format!("File: {relative_path}\nTipo: {ext}\nLabels UI: {labels_str}\n\n");

        let chunks: Vec<String> = if content.len() <= CHUNK_SIZE {
            let text = format!("{header}{}", &content[..content.len().min(2000)]);
            vec![text]
        } else {
            let mut result = Vec::new();
            let chars: Vec<char> = content.chars().collect();
            let mut start = 0;
            while start < chars.len() && result.len() < MAX_CHUNKS_PER_FILE {
                let end = (start + CHUNK_SIZE).min(chars.len());
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
                start = end.saturating_sub(CHUNK_OVERLAP);
            }
            result
        };

        for (chunk_index, chunk_text) in chunks.iter().enumerate() {
            let embedding = match state.orchestrator.embed_text(chunk_text).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        "code index: embed failed for {relative_path} chunk {chunk_index}: {e}"
                    );
                    failed += 1;
                    if first_error.is_none() {
                        first_error = Some(format!("embed fallito per '{relative_path}': {e}"));
                    }
                    continue;
                }
            };

            let mut hasher = Sha256::new();
            hasher.update(project_id.as_bytes());
            hasher.update(b":code:");
            hasher.update(relative_path.as_bytes());
            hasher.update(b":");
            hasher.update(chunk_index.to_string().as_bytes());
            let hash_bytes = hasher.finalize();
            let uuid_bytes: [u8; 16] = hash_bytes[..16].try_into().expect("sha256>=16");
            let point_id = uuid::Uuid::from_bytes(uuid_bytes).to_string();

            let payload = json!({
                "project_id": project_id.to_string(),
                "type": "code_file",
                "file_path": relative_path,
                "chunk_index": chunk_index,
                "ui_labels": ui_labels,
                "file_ext": ext,
                "text": chunk_text,
                "active": true,
                "indexed_at": chrono::Utc::now().to_rfc3339(),
            });

            match vector_memory::upsert_code_index_point(&state.db, &point_id, &embedding, payload)
                .await
            {
                Ok(()) => chunks_indexed += 1,
                Err(e) => {
                    tracing::warn!(
                        "code index: upsert failed for {relative_path} chunk {chunk_index}: {e}"
                    );
                    failed += 1;
                    if first_error.is_none() {
                        first_error = Some(format!("upsert fallito per '{relative_path}': {e}"));
                    }
                }
            }
        }

        files_processed += 1;
    }

    let status = if chunks_indexed > 0 {
        if failed > 0 {
            "partial"
        } else {
            "indexed"
        }
    } else if failed > 0 {
        "error"
    } else {
        "skipped"
    };

    json!({
        "status": status,
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
pub async fn reindex_single_file_inner(
    db: &sqlx::PgPool,
    neural: &crate::orchestrator::NeuralCoreClient,
    project_id: Uuid,
    root: &Path,
    file_path: &Path,
    force: bool,
) -> anyhow::Result<usize> {
    const CODE_EXTENSIONS: &[&str] = &["tsx", "jsx", "ts", "js", "rs", "py", "cs", "go", "vue"];
    const MAX_FILE_BYTES: u64 = 200 * 1024;
    const MAX_CHUNKS_PER_FILE: usize = 10;
    const CHUNK_SIZE: usize = 2000;
    const CHUNK_OVERLAP: usize = 200;

    let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if !CODE_EXTENSIONS.contains(&ext) {
        return Ok(0);
    }

    if let Ok(meta) = tokio::fs::metadata(file_path).await {
        if meta.len() > MAX_FILE_BYTES {
            return Ok(0);
        }
    }

    let relative_path = file_path
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| file_path.to_string_lossy().replace('\\', "/"));

    let content = match tokio::fs::read_to_string(file_path).await {
        Ok(c) => c,
        Err(e) => return Err(anyhow::anyhow!("cannot read {relative_path}: {e}")),
    };

    // Calcola hash SHA256 del contenuto e salta se invariato
    let content_hash = {
        let mut h = Sha256::new();
        h.update(content.as_bytes());
        format!("{:x}", h.finalize())
    };
    let stored_hash: Option<String> = sqlx::query_scalar(
        "SELECT content_hash FROM file_index_hashes WHERE project_id = $1 AND file_path = $2",
    )
    .bind(project_id)
    .bind(&relative_path)
    .fetch_optional(db)
    .await
    .unwrap_or(None);

    if !force && stored_hash.as_deref() == Some(&content_hash) {
        tracing::debug!("reindex_single_file: {relative_path} — hash invariato, skip");
        return Ok(0);
    }

    // Cancella i chunk precedenti di questo file
    vector_memory::delete_code_index_file_points(db, project_id, &relative_path)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("reindex_single_file: cleanup failed for {relative_path}: {e}")
        });

    // Estrai UI labels per tsx/jsx/vue
    let ui_labels: Vec<String> = if matches!(ext, "tsx" | "jsx" | "vue") {
        let mut labels: Vec<String> = Vec::new();
        let re_jsx = regex::Regex::new(r">\s*([A-Za-zÀ-ÿ][^<>{}\n]{3,60})\s*<").unwrap();
        for cap in re_jsx.captures_iter(&content) {
            let label = cap[1].trim().to_string();
            if !labels.contains(&label) {
                labels.push(label);
            }
            if labels.len() >= 20 {
                break;
            }
        }
        if labels.len() < 20 {
            let re_props = regex::Regex::new(
                r#"(?:title|label|placeholder|aria-label)=["']([^"']{3,60})["']"#,
            )
            .unwrap();
            for cap in re_props.captures_iter(&content) {
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
    } else {
        Vec::new()
    };

    let labels_str = ui_labels.join(", ");
    let header = format!("File: {relative_path}\nTipo: {ext}\nLabels UI: {labels_str}\n\n");

    let chunks: Vec<String> = if content.len() <= CHUNK_SIZE {
        vec![format!("{header}{}", &content[..content.len().min(2000)])]
    } else {
        let mut result = Vec::new();
        let chars: Vec<char> = content.chars().collect();
        let mut start = 0;
        while start < chars.len() && result.len() < MAX_CHUNKS_PER_FILE {
            let end = (start + CHUNK_SIZE).min(chars.len());
            let chunk_content: String = chars[start..end].iter().collect();
            let idx = result.len();
            let text = if idx == 0 {
                format!("{header}{chunk_content}")
            } else {
                format!("File: {relative_path}\nChunk: {idx}\n\n{chunk_content}")
            };
            result.push(text);
            if end >= chars.len() {
                break;
            }
            start = end.saturating_sub(CHUNK_OVERLAP);
        }
        result
    };

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
        let mut hasher = Sha256::new();
        hasher.update(project_id.as_bytes());
        hasher.update(b":code:");
        hasher.update(relative_path.as_bytes());
        hasher.update(b":");
        hasher.update(chunk_index.to_string().as_bytes());
        let hash_bytes2 = hasher.finalize();
        let uuid_bytes2: [u8; 16] = hash_bytes2[..16].try_into().expect("sha256>=16");
        let point_id = uuid::Uuid::from_bytes(uuid_bytes2).to_string();

        let payload = json!({
            "project_id": project_id.to_string(),
            "type": "code_file",
            "file_path": relative_path,
            "chunk_index": chunk_index,
            "ui_labels": ui_labels,
            "file_ext": ext,
            "text": chunk_text,
            "active": true,
            "indexed_at": chrono::Utc::now().to_rfc3339(),
        });

        if let Ok(()) =
            vector_memory::upsert_code_index_point(db, &point_id, &embedding, payload).await
        {
            indexed += 1;
        }
    }

    // Aggiorna hash nel DB se almeno un chunk e' stato indicizzato
    if indexed > 0 {
        sqlx::query(
            "INSERT INTO file_index_hashes (project_id, file_path, content_hash, indexed_at)
             VALUES ($1, $2, $3, NOW())
             ON CONFLICT (project_id, file_path) DO UPDATE
               SET content_hash = EXCLUDED.content_hash, indexed_at = NOW()",
        )
        .bind(project_id)
        .bind(&relative_path)
        .bind(&content_hash)
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
        &relative_path,
        &content,
    )
    .await;
    // Segnala al code-docs enricher che il sorgente e' cambiato: marca stale il
    // doc kind=code cosi' il worker rigenera la scheda (best-effort, mig 0331).
    crate::wiki::code_docs_enricher::mark_code_doc_stale_if_changed(
        db,
        project_id,
        &relative_path,
        &content,
    )
    .await;
    let _ = (root, &content_hash);

    tracing::debug!("reindex_single_file: {relative_path} → {indexed} chunks");
    Ok(indexed)
}

/// Restituisce lo stato dell'indice vettoriale: file modificati vs indicizzati
pub async fn get_index_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let row = sqlx::query_as::<_, (String, Option<Uuid>)>(
        "SELECT COALESCE(r.root_path, p.analysis_json->>'rootPath', ''), p.owner_user_id FROM projects p LEFT JOIN repositories r ON r.project_id = p.id WHERE p.id = $1"
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Project not found".to_string()))?;

    let (root_path, owner_id) = row;
    let caller_id = parse_user_id(&claims).map_err(|e| {
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

    const CODE_EXTENSIONS: &[&str] = &["tsx", "jsx", "ts", "js", "rs", "py", "cs", "go", "vue"];
    const MAX_FILE_BYTES: u64 = 200 * 1024;

    let stored: Vec<(String, String)> = sqlx::query_as(
        "SELECT file_path, content_hash FROM file_index_hashes WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let stored_map: std::collections::HashMap<String, String> = stored.into_iter().collect();

    let source_files = collect_source_files(&root_path, CODE_EXTENSIONS);

    let mut stale: Vec<String> = Vec::new();
    let mut up_to_date = 0usize;
    let mut not_indexed = 0usize;

    for abs_path in &source_files {
        let rel = abs_path
            .strip_prefix(&root_path)
            .unwrap_or(abs_path)
            .trim_start_matches(['/', '\\']);
        let rel = rel.replace('\\', "/");

        if let Ok(meta) = std::fs::metadata(abs_path) {
            if meta.len() > MAX_FILE_BYTES {
                continue;
            }
        } else {
            continue;
        }

        let current_hash = match std::fs::read(abs_path) {
            Ok(bytes) => {
                let mut h = Sha256::new();
                h.update(&bytes);
                format!("{:x}", h.finalize())
            }
            Err(_) => continue,
        };

        match stored_map.get(&rel) {
            Some(h) if h == &current_hash => {
                up_to_date += 1;
            }
            Some(_) => {
                stale.push(rel);
            }
            None => {
                not_indexed += 1;
                stale.push(rel);
            }
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
pub async fn reindex_stale_files(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
    Query(q): Query<ReindexQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let row = sqlx::query_as::<_, (String, Option<Uuid>)>(
        "SELECT COALESCE(r.root_path, p.analysis_json->>'rootPath', ''), p.owner_user_id FROM projects p LEFT JOIN repositories r ON r.project_id = p.id WHERE p.id = $1"
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Project not found".to_string()))?;

    let (root_path, owner_id) = row;
    let caller_id = parse_user_id(&claims).map_err(|e| {
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

    const CODE_EXTENSIONS: &[&str] = &["tsx", "jsx", "ts", "js", "rs", "py", "cs", "go", "vue"];
    const MAX_FILE_BYTES: u64 = 200 * 1024;

    let stored: Vec<(String, String)> = sqlx::query_as(
        "SELECT file_path, content_hash FROM file_index_hashes WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let stored_map: std::collections::HashMap<String, String> = stored.into_iter().collect();

    let source_files = collect_source_files(&root_path, CODE_EXTENSIONS);
    let root_path_obj = std::path::Path::new(&root_path);

    let mut reindexed = 0usize;
    let mut skipped = 0usize;

    for abs_path_str in &source_files {
        let abs_path = std::path::Path::new(abs_path_str);
        if let Ok(meta) = std::fs::metadata(abs_path) {
            if meta.len() > MAX_FILE_BYTES {
                continue;
            }
        } else {
            continue;
        }

        let rel = abs_path
            .strip_prefix(root_path_obj)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| abs_path_str.to_string());

        let current_hash = match std::fs::read(abs_path) {
            Ok(bytes) => {
                let mut h = Sha256::new();
                h.update(&bytes);
                format!("{:x}", h.finalize())
            }
            Err(_) => continue,
        };

        if !q.force
            && stored_map
                .get(&rel)
                .map(|h| h == &current_hash)
                .unwrap_or(false)
        {
            skipped += 1;
            continue;
        }

        match reindex_single_file_inner(
            &state.db,
            &state.orchestrator.neural,
            project_id,
            root_path_obj,
            abs_path,
            q.force,
        )
        .await
        {
            Ok(n) if n > 0 => {
                reindexed += 1;
            }
            _ => {}
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
            return;
        }
    };

    let root = std::path::PathBuf::from(&root_str);
    if !root.exists() {
        tracing::warn!(
            "spawn_code_index_if_needed: project={project_id} root={root_str} non esiste, skip"
        );
        return;
    }

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
    let state_bg = state.clone();
    let root_bg = root.clone();
    tracing::info!(
        "spawn_code_index_if_needed: avvio indicizzazione project={project_id} root={root_str}"
    );
    tokio::spawn(async move {
        let result = index_project_code_files(&state_bg, project_id, &root_bg).await;
        state_bg.indexing_projects.remove(&project_id);
        tracing::info!(
            "spawn_code_index_if_needed: completata project={project_id} status={}",
            result.get("status").and_then(|v| v.as_str()).unwrap_or("?")
        );
        // Avvia il file watcher ora che l'indice iniziale e' pronto.
        crate::projects::file_watcher::spawn_file_watcher(&state_bg, project_id, root_bg);
    });
}
