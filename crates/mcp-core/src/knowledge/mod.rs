// ═══════════════════════════════════════════════════════════════════════════
// knowledge/mod.rs — Knowledge Base per-progetto (Obsidian-compatible)
// ═══════════════════════════════════════════════════════════════════════════

pub mod vault;
pub mod routes;
pub mod generators;
pub mod functional_spec_agent;
pub mod graph_import;
pub mod code_graph;
pub mod auto_link;
pub mod ingest_run;
pub mod impact;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

/// Normalizza la prima riga del contenuto come titolo (max `max_len` caratteri).
pub fn title_from_content(content: &str, max_len: usize) -> String {
    let first_line = content.lines().next().unwrap_or(content);
    // Rimuovi markdown heading markers
    let cleaned = first_line
        .trim_start_matches('#')
        .trim_start_matches('*')
        .trim();
    if cleaned.len() > max_len {
        format!("{}...", &cleaned[..max_len.min(cleaned.len())])
    } else if cleaned.is_empty() {
        "Nota senza titolo".to_string()
    } else {
        cleaned.to_string()
    }
}

/// Genera slug kebab-case dal titolo (max `max_len` caratteri).
pub fn slug_from_title(title: &str, max_len: usize) -> String {
    let slug: String = title
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c
            } else {
                '-'
            }
        })
        .collect();
    // Rimuovi trattini consecutivi e trim
    let mut result = String::with_capacity(slug.len());
    let mut prev_dash = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_dash && !result.is_empty() {
                result.push('-');
            }
            prev_dash = true;
        } else {
            result.push(c);
            prev_dash = false;
        }
    }
    let trimmed = result.trim_end_matches('-');
    if trimmed.len() > max_len {
        trimmed[..max_len].trim_end_matches('-').to_string()
    } else {
        trimmed.to_string()
    }
}

/// Estrai tag letterali `#xxx` dal contenuto.
pub fn extract_tags(content: &str) -> Vec<String> {
    let mut tags = Vec::new();
    for word in content.split_whitespace() {
        if word.starts_with('#') && word.len() > 1 {
            let tag = word
                .trim_start_matches('#')
                .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-');
            if !tag.is_empty() && tag.len() <= 50 {
                let t = tag.to_lowercase();
                if !tags.contains(&t) {
                    tags.push(t);
                }
            }
        }
    }
    tags
}

/// Hash SHA-256 di un contenuto (usato per vault_file_hash).
pub fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Crea automaticamente una nota dalla richiesta utente.
/// Eseguito in `tokio::spawn` per non bloccare il turno chat.
pub async fn create_note_from_user_message(
    db: PgPool,
    neural: crate::orchestrator::NeuralCoreClient,
    project_id: Uuid,
    message_id: Uuid,
    content: String,
    intent: Option<String>,
    repo_root: Option<String>,
    project_channels: nexus_events::ProjectChannels,
) {
    if let Err(e) = create_note_inner(
        &db,
        &neural,
        project_id,
        message_id,
        &content,
        intent.as_deref(),
        repo_root.as_deref(),
        &project_channels,
    )
    .await
    {
        tracing::warn!(
            project_id = %project_id,
            message_id = %message_id,
            "knowledge auto-create fallita: {e}"
        );
    }
}

pub async fn create_note_inner(
    db: &PgPool,
    neural: &crate::orchestrator::NeuralCoreClient,
    project_id: Uuid,
    message_id: Uuid,
    content: &str,
    intent: Option<&str>,
    repo_root: Option<&str>,
    project_channels: &nexus_events::ProjectChannels,
) -> anyhow::Result<()> {
    use anyhow::Context;

    let note_id = Uuid::new_v4();
    let title = title_from_content(content, 80);
    let tags = extract_tags(content);

    // Embedding del contenuto (troncato a 2000 caratteri)
    let embed_text = if content.len() > 2000 {
        &content[..2000]
    } else {
        content
    };
    let embed = neural.embed_text("", embed_text).await?;
    let point_id = Uuid::new_v4().to_string();

    // Upsert Qdrant
    let payload = json!({
        "project_id": project_id.to_string(),
        "note_id": note_id.to_string(),
        "intent": intent.unwrap_or("unknown"),
        "status": "draft",
    });
    crate::vector_memory::upsert_knowledge_point(db, &point_id, embed, payload).await?;

    // Calcola vault_file_path e scrivi file .md
    let now = chrono::Utc::now();
    let slug = slug_from_title(&title, 60);
    let date_prefix = now.format("%Y-%m-%d-%H%M").to_string();
    let filename = format!("{date_prefix}-{slug}.md");
    let rel_path = format!(
        ".nexus/knowledge/notes/{}/{}/{}",
        now.format("%Y"),
        now.format("%m"),
        filename
    );

    let body_md = content.to_string();
    let vault_content = vault::serialize_note(
        note_id,
        project_id,
        Some(message_id),
        None,
        intent,
        "draft",
        &tags,
        &[],
        &now,
        &now,
        &title,
        &body_md,
        &[],
    );
    let vault_hash = sha256_hex(&vault_content);

    // Scrivi su filesystem se repo_root disponibile
    let mut vault_file_path: Option<String> = None;
    if let Some(root) = repo_root {
        let full_path = format!("{root}/{rel_path}");
        if let Some(parent) = std::path::Path::new(&full_path).parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        if tokio::fs::write(&full_path, &vault_content).await.is_ok() {
            vault_file_path = Some(rel_path.clone());

            // Assicura .gitignore contenga .nexus/ (se commit_vault_to_git = false)
            let commit_to_git = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ")
                .bind("knowledge.commit_vault_to_git")
                .fetch_optional(db)
                .await
                .ok()
                .flatten()
                .map(|v| v.trim() == "true")
                .unwrap_or(false);
            if !commit_to_git {
                vault::ensure_gitignore_entry(root).await;
            }
        }
    }

    // Insert DB (idempotente grazie a idx_pkn_msg_unique)
    sqlx::query(
        r#"
        INSERT INTO project_knowledge_notes (
            id, project_id, source_message_id, intent, title, body_md,
            status, qdrant_point_id, tags, vault_file_path, vault_file_hash,
            created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, 'draft', $7, $8, $9, $10, NOW(), NOW())
        ON CONFLICT (source_message_id) WHERE source_message_id IS NOT NULL
        DO NOTHING
        "#,
    )
    .bind(note_id)
    .bind(project_id)
    .bind(message_id)
    .bind(intent)
    .bind(&title)
    .bind(&body_md)
    .bind(&point_id)
    .bind(&tags)
    .bind(&vault_file_path)
    .bind(&vault_hash)
    .execute(db)
    .await
    .context("insert nota knowledge fallito")?;

    // Upsert tags
    for tag in &tags {
        sqlx::query(
            r#"
            INSERT INTO project_knowledge_tags (project_id, tag, note_count, last_used_at)
            VALUES ($1, $2, 1, NOW())
            ON CONFLICT (project_id, tag) DO UPDATE
            SET note_count = project_knowledge_tags.note_count + 1,
                last_used_at = NOW()
            "#,
        )
        .bind(project_id)
        .bind(tag)
        .execute(db)
        .await
        .ok();
    }

    // Emit SSE
    let _ = nexus_events::dispatcher::emit(
        project_channels,
        project_id,
        nexus_events::ProjectEvent::KnowledgeNoteCreated {
            note_id,
            title: title.clone(),
            intent: intent.map(|s| s.to_string()),
        },
    );

    tracing::debug!(
        project_id = %project_id,
        note_id = %note_id,
        title = %title,
        "nota knowledge creata automaticamente"
    );

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Seed knowledge base da analisi profonda progetto
// ═══════════════════════════════════════════════════════════════════════════

/// Popola la knowledge base con le informazioni estratte dall'analisi profonda.
///
/// Crea fino a 3 categorie di note:
/// 1. **Panoramica progetto** (intent=`analysis`): sommario + architettura + dominio.
/// 2. **Problemi di configurazione** (intent=`fix`): una nota per ogni `config_issue`.
/// 3. **Azioni suggerite** (intent=`improvement`): una nota per ogni `suggested_action`.
///
/// Tutte le note nascono con `status='active'` (l'analisi e' un atto completato, non draft).
pub async fn seed_knowledge_from_insights(
    db: PgPool,
    neural: crate::orchestrator::NeuralCoreClient,
    project_id: Uuid,
    insights: Value,
    repo_root: Option<String>,
    project_channels: nexus_events::ProjectChannels,
) {
    if let Err(e) = seed_knowledge_inner(
        &db, &neural, project_id, &insights, repo_root.as_deref(), &project_channels,
    ).await {
        tracing::warn!(
            project_id = %project_id,
            "seed knowledge da analisi fallito: {e}"
        );
    }
}

async fn seed_knowledge_inner(
    db: &PgPool,
    neural: &crate::orchestrator::NeuralCoreClient,
    project_id: Uuid,
    insights: &Value,
    repo_root: Option<&str>,
    project_channels: &nexus_events::ProjectChannels,
) -> anyhow::Result<()> {
    use anyhow::Context;

    // --- 1. Nota panoramica progetto ---
    let summary = insights.get("project_summary").and_then(|v| v.as_str()).unwrap_or("");
    let domain = insights.get("domain").and_then(|v| v.as_str()).unwrap_or("");
    let arch = insights.get("architecture").cloned().unwrap_or(json!({}));
    let arch_pattern = arch.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
    let arch_desc = arch.get("description").and_then(|v| v.as_str()).unwrap_or("");
    let primary_langs: Vec<&str> = arch.get("primary_languages")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let primary_fw: Vec<&str> = arch.get("primary_frameworks")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    if !summary.is_empty() {
        let overview_body = format!(
            "## Sommario\n\n{}\n\n## Dominio\n\n{}\n\n## Architettura\n\n- **Pattern**: {}\n- **Descrizione**: {}\n- **Linguaggi**: {}\n- **Framework**: {}",
            summary,
            if domain.is_empty() { "Non specificato" } else { domain },
            if arch_pattern.is_empty() { "N/D" } else { arch_pattern },
            if arch_desc.is_empty() { "N/D" } else { arch_desc },
            if primary_langs.is_empty() { "N/D".to_string() } else { primary_langs.join(", ") },
            if primary_fw.is_empty() { "N/D".to_string() } else { primary_fw.join(", ") },
        );
        let overview_title = format!("Panoramica progetto: {}", &summary[..summary.len().min(50)]);
        let mut tags = vec!["analisi-progetto".to_string(), "panoramica".to_string()];
        if !domain.is_empty() { tags.push(domain.to_lowercase()); }
        for lang in &primary_langs { tags.push(lang.to_lowercase()); }

        insert_seed_note(
            db, neural, project_id, &overview_title, &overview_body,
            "analysis", &tags, &[], repo_root, project_channels,
        ).await.ok();
    }

    // --- 2. Note per config_issues ---
    if let Some(issues) = insights.get("config_issues").and_then(|v| v.as_array()) {
        for issue in issues {
            let severity = issue.get("severity").and_then(|v| v.as_str()).unwrap_or("medium");
            let title_raw = issue.get("title").and_then(|v| v.as_str()).unwrap_or("Problema di configurazione");
            let description = issue.get("description").and_then(|v| v.as_str()).unwrap_or("");
            let suggested_fix = issue.get("suggested_fix").and_then(|v| v.as_str()).unwrap_or("");
            let files: Vec<String> = issue.get("files")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();

            let body = format!(
                "## Problema\n\n{}\n\n## Severita\n\n**{}**\n\n## Fix suggerito\n\n{}\n\n## File coinvolti\n\n{}",
                description,
                severity,
                if suggested_fix.is_empty() { "Nessun fix suggerito" } else { suggested_fix },
                if files.is_empty() { "Nessuno".to_string() } else { files.iter().map(|f| format!("- `{f}`")).collect::<Vec<_>>().join("\n") },
            );
            let title = format!("[{severity}] {title_raw}");
            let tags = vec![
                "analisi-progetto".to_string(),
                "config-issue".to_string(),
                severity.to_lowercase(),
            ];

            insert_seed_note(
                db, neural, project_id, &title, &body,
                "fix", &tags, &files, repo_root, project_channels,
            ).await.ok();
        }
    }

    // --- 3. Note per suggested_actions ---
    if let Some(actions) = insights.get("suggested_actions").and_then(|v| v.as_array()) {
        for action in actions {
            let priority = action.get("priority").and_then(|v| v.as_str()).unwrap_or("medium");
            let title_raw = action.get("title").and_then(|v| v.as_str()).unwrap_or("Azione suggerita");
            let rationale = action.get("rationale").and_then(|v| v.as_str()).unwrap_or("");
            let command = action.get("command").and_then(|v| v.as_str()).unwrap_or("");

            let body = format!(
                "## Azione\n\n{}\n\n## Priorita\n\n**{}**\n\n## Motivazione\n\n{}\n\n## Comando suggerito\n\n```\n{}\n```",
                title_raw,
                priority,
                if rationale.is_empty() { "Non specificata" } else { rationale },
                if command.is_empty() { "N/D" } else { command },
            );
            let title = format!("[{priority}] {title_raw}");
            let tags = vec![
                "analisi-progetto".to_string(),
                "azione-suggerita".to_string(),
                priority.to_lowercase(),
            ];

            insert_seed_note(
                db, neural, project_id, &title, &body,
                "improvement", &tags, &[], repo_root, project_channels,
            ).await.ok();
        }
    }

    let note_count = 1 // panoramica
        + insights.get("config_issues").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0)
        + insights.get("suggested_actions").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
    tracing::info!(
        project_id = %project_id,
        note_count,
        "knowledge base inizializzata da analisi progetto"
    );

    Ok(())
}

/// Helper interno: inserisce una singola nota di seed (embedding + Qdrant + vault + DB + SSE).
async fn insert_seed_note(
    db: &PgPool,
    neural: &crate::orchestrator::NeuralCoreClient,
    project_id: Uuid,
    title: &str,
    body_md: &str,
    intent: &str,
    tags: &[String],
    file_paths: &[String],
    repo_root: Option<&str>,
    project_channels: &nexus_events::ProjectChannels,
) -> anyhow::Result<()> {
    use anyhow::Context;

    let note_id = Uuid::new_v4();

    // Embedding
    let embed_text = if body_md.len() > 2000 { &body_md[..2000] } else { body_md };
    let embed = neural.embed_text("", embed_text).await?;
    let point_id = Uuid::new_v4().to_string();

    // Qdrant upsert
    let payload = json!({
        "project_id": project_id.to_string(),
        "note_id": note_id.to_string(),
        "intent": intent,
        "status": "active",
    });
    crate::vector_memory::upsert_knowledge_point(db, &point_id, embed, payload).await?;

    // Vault file
    let now = chrono::Utc::now();
    let slug = slug_from_title(title, 60);
    let date_prefix = now.format("%Y-%m-%d-%H%M").to_string();
    let filename = format!("{date_prefix}-{slug}.md");
    let rel_path = format!(
        ".nexus/knowledge/notes/{}/{}/{}",
        now.format("%Y"),
        now.format("%m"),
        filename
    );

    let vault_content = vault::serialize_note(
        note_id,
        project_id,
        None,  // nessun source_message_id
        None,  // nessun source_run_id
        Some(intent),
        "active",
        tags,
        file_paths,
        &now,
        &now,
        title,
        body_md,
        &[],
    );
    let vault_hash = sha256_hex(&vault_content);

    let mut vault_file_path: Option<String> = None;
    if let Some(root) = repo_root {
        let full_path = format!("{root}/{rel_path}");
        if let Some(parent) = std::path::Path::new(&full_path).parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        if tokio::fs::write(&full_path, &vault_content).await.is_ok() {
            vault_file_path = Some(rel_path);
        }
    }

    // DB insert (nessun source_message_id, quindi nessun conflitto su idx_pkn_msg_unique)
    sqlx::query(
        r#"
        INSERT INTO project_knowledge_notes (
            id, project_id, intent, title, body_md,
            status, qdrant_point_id, tags, file_paths,
            vault_file_path, vault_file_hash,
            created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, 'active', $6, $7, $8, $9, $10, NOW(), NOW())
        "#,
    )
    .bind(note_id)
    .bind(project_id)
    .bind(intent)
    .bind(title)
    .bind(body_md)
    .bind(&point_id)
    .bind(tags)
    .bind(file_paths)
    .bind(&vault_file_path)
    .bind(&vault_hash)
    .execute(db)
    .await
    .context("insert nota seed knowledge fallito")?;

    // Upsert tags
    for tag in tags {
        sqlx::query(
            r#"
            INSERT INTO project_knowledge_tags (project_id, tag, note_count, last_used_at)
            VALUES ($1, $2, 1, NOW())
            ON CONFLICT (project_id, tag) DO UPDATE
            SET note_count = project_knowledge_tags.note_count + 1,
                last_used_at = NOW()
            "#,
        )
        .bind(project_id)
        .bind(tag)
        .execute(db)
        .await
        .ok();
    }

    // SSE
    let _ = nexus_events::dispatcher::emit(
        project_channels,
        project_id,
        nexus_events::ProjectEvent::KnowledgeNoteCreated {
            note_id,
            title: title.to_string(),
            intent: Some(intent.to_string()),
        },
    );

    tracing::debug!(
        project_id = %project_id,
        note_id = %note_id,
        intent,
        "nota seed knowledge creata da analisi progetto"
    );

    Ok(())
}

/// Promuove le note da 'draft' a 'active' quando il run agente completa.
pub async fn promote_notes_on_run_completed(
    db: &PgPool,
    run_id: Uuid,
    files_touched: &[String],
    project_channels: &nexus_events::ProjectChannels,
    project_id: Uuid,
) {
    let result = sqlx::query_scalar::<_, Uuid>(
        r#"
        UPDATE project_knowledge_notes
        SET status = 'active',
            updated_at = NOW(),
            file_paths = $2
        WHERE source_run_id = $1 AND status = 'draft'
        RETURNING id
        "#,
    )
    .bind(run_id)
    .bind(files_touched)
    .fetch_all(db)
    .await;

    if let Ok(ids) = result {
        for note_id in ids {
            let _ = nexus_events::dispatcher::emit(
                project_channels,
                project_id,
                nexus_events::ProjectEvent::KnowledgeNoteUpdated {
                    note_id,
                    status: "active".to_string(),
                },
            );
        }
    }
}
