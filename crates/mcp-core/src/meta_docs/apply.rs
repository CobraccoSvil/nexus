// ═══════════════════════════════════════════════════════════════════════════
// meta_docs/apply.rs — Pipeline di applicazione doc generate
//
// Per ogni `GeneratedDoc`:
//   1. Calcola sha256(body con frontmatter)
//   2. Confronta con vault_file_hash in DB
//   3. Se diff: UPDATE DB + write filesystem + (TODO) emit SSE
//   4. Else: skip (idempotente)
//
// Inoltre, scrive su disco l'intera nota come Markdown + frontmatter YAML.
// ═══════════════════════════════════════════════════════════════════════════

use crate::meta_docs::generators::GeneratedDoc;
use crate::meta_docs::vault::{self, VaultMetaLink};
use crate::AppState;
use anyhow::{Context, Result};
use chrono::Utc;
use sqlx::Row;
use uuid::Uuid;

/// Applica una doc generata: UPSERT in DB + write filesystem.
///
/// Restituisce `(doc_id, was_updated)` dove `was_updated=true` se il contenuto
/// e' cambiato (e quindi e' stato scritto su disco), `false` se idempotente.
pub async fn apply_generated_doc(
    state: &AppState,
    vault_root: &str,
    doc: &GeneratedDoc,
) -> Result<(Uuid, bool)> {
    // Serializza il body completo con frontmatter
    let doc_id_new = Uuid::new_v4();
    let now = Utc::now();

    // Cerca eventuale doc esistente con stesso vault_file_path
    let existing_row = sqlx::query(
        "SELECT id, vault_file_hash, auto_generated FROM nexus_meta_docs WHERE vault_file_path = $1",
    )
    .bind(&doc.vault_file_path)
    .fetch_optional(&state.db)
    .await
    .context("query nexus_meta_docs by path")?;

    let (doc_id, created_at_existing, was_user_curated) = if let Some(row) = &existing_row {
        let id: Uuid = row.try_get("id")?;
        let user_curated: bool = !row.try_get::<bool, _>("auto_generated").unwrap_or(true);
        let created_at_existing: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
            "SELECT created_at FROM nexus_meta_docs WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&state.db)
        .await
        .unwrap_or(now);
        (id, created_at_existing, user_curated)
    } else {
        (doc_id_new, now, false)
    };

    // Se la doc esistente e' stata curata manualmente (auto_generated=false), NON sovrascrivere
    if was_user_curated {
        return Ok((doc_id, false));
    }

    let body_full = vault::serialize_meta_doc(
        doc_id,
        &doc.kind,
        &doc.title,
        &doc.slug,
        &doc.tags,
        doc.source_commit.as_deref(),
        &doc.source_files,
        true, // auto_generated
        &created_at_existing,
        &doc.now,
        &doc.body_md,
        &[] as &[VaultMetaLink],
    );

    let new_hash = vault::sha256_hex(&body_full);

    let old_hash_opt: Option<String> = existing_row
        .as_ref()
        .and_then(|r| r.try_get::<String, _>("vault_file_hash").ok());

    if let Some(old_hash) = &old_hash_opt {
        if old_hash == &new_hash {
            return Ok((doc_id, false));
        }
    }

    // Scrivi su disco
    let full_path = format!("{}/{}", vault_root.trim_end_matches('/'), doc.vault_file_path);
    if let Some(parent) = std::path::Path::new(&full_path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    tokio::fs::write(&full_path, &body_full)
        .await
        .with_context(|| format!("scrittura {full_path}"))?;

    // UPSERT in DB
    if existing_row.is_some() {
        sqlx::query(
            r#"
            UPDATE nexus_meta_docs SET
                kind = $1,
                title = $2,
                slug = $3,
                body_md = $4,
                vault_file_hash = $5,
                source_commit = $6,
                source_files = $7,
                tags = $8,
                auto_generated = TRUE,
                updated_at = NOW()
            WHERE id = $9
            "#,
        )
        .bind(&doc.kind)
        .bind(&doc.title)
        .bind(&doc.slug)
        .bind(&doc.body_md)
        .bind(&new_hash)
        .bind(doc.source_commit.as_deref())
        .bind(&doc.source_files)
        .bind(&doc.tags)
        .bind(doc_id)
        .execute(&state.db)
        .await
        .context("UPDATE nexus_meta_docs")?;
    } else {
        sqlx::query(
            r#"
            INSERT INTO nexus_meta_docs
                (id, kind, title, slug, body_md, vault_file_path, vault_file_hash,
                 source_commit, source_files, tags, auto_generated)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, TRUE)
            "#,
        )
        .bind(doc_id)
        .bind(&doc.kind)
        .bind(&doc.title)
        .bind(&doc.slug)
        .bind(&doc.body_md)
        .bind(&doc.vault_file_path)
        .bind(&new_hash)
        .bind(doc.source_commit.as_deref())
        .bind(&doc.source_files)
        .bind(&doc.tags)
        .execute(&state.db)
        .await
        .context("INSERT nexus_meta_docs")?;
    }

    // Genera embedding + upsert in Qdrant `nexus_meta_docs` per linking semantico
    let embed_text = if doc.body_md.len() > 2000 {
        &doc.body_md[..2000]
    } else {
        doc.body_md.as_str()
    };
    let combined = format!("{}\n\n{}", doc.title, embed_text);
    match state.orchestrator.neural.embed_text("", &combined).await {
        Ok(vector) => {
            let point_id = doc_id.to_string();
            let payload = serde_json::json!({
                "doc_id": doc_id.to_string(),
                "kind": doc.kind,
                "slug": doc.slug,
                "title": doc.title,
            });
            if let Err(e) = crate::vector_memory::upsert_meta_doc_point(
                &state.db,
                &point_id,
                vector,
                payload,
            )
            .await
            {
                tracing::debug!(slug = %doc.slug, error = %e, "meta-doc embed upsert fallito");
            } else {
                let _ = sqlx::query(
                    "UPDATE nexus_meta_docs SET qdrant_point_id = $1 WHERE id = $2",
                )
                .bind(&point_id)
                .bind(doc_id)
                .execute(&state.db)
                .await;
            }
        }
        Err(e) => {
            tracing::debug!(slug = %doc.slug, error = %e, "meta-doc embed fallito");
        }
    }

    Ok((doc_id, true))
}

/// Risolve il vault_root assoluto dalle settings + repo root (di default `/home/administrator/ideai/`).
pub async fn resolve_vault_root(state: &AppState) -> String {
    let vault_rel: String = sqlx::query_scalar(
        "SELECT value FROM settings WHERE key = 'meta_docs.vault_path'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| "docs/.nexus-vault".to_string());

    let repo_root = std::env::var("NEXUS_REPO_ROOT")
        .unwrap_or_else(|_| "/home/administrator/ideai".to_string());

    format!("{}/{}", repo_root.trim_end_matches('/'), vault_rel)
}
