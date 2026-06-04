// ═══════════════════════════════════════════════════════════════════════════
// meta_docs/routes.rs — Endpoint REST per il meta-docs vault
//
// Endpoint base (read-only):
//   GET  /api/meta-docs/list
//   GET  /api/meta-docs/:id
//
// Endpoint scrittura (negli step successivi):
//   POST /api/meta-docs/ingest-commit       (hook lefthook post-commit)
//   POST /api/meta-docs/refresh-all         (refresh manuale completo)
//   PATCH /api/meta-docs/:id                (edit manuale via UI)
// ═══════════════════════════════════════════════════════════════════════════

use crate::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct MetaDocSummary {
    pub id: Uuid,
    pub kind: String,
    pub title: String,
    pub slug: String,
    pub vault_file_path: String,
    pub tags: Vec<String>,
    pub auto_generated: bool,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub kind: Option<String>,
    pub q: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// `GET /api/meta-docs/list?kind=adr&q=routing&limit=20`
pub async fn list_meta_docs(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let offset = q.offset.unwrap_or(0).max(0);

    let kind_filter = q.kind.unwrap_or_default();
    let q_filter = q.q.unwrap_or_default();

    let rows = sqlx::query(
        r#"
        SELECT id, kind, title, slug, vault_file_path, tags, auto_generated, updated_at
        FROM nexus_meta_docs
        WHERE ($1 = '' OR kind = $1)
          AND (
              $2 = ''
              OR to_tsvector('simple', title || ' ' || body_md)
                 @@ plainto_tsquery('simple', $2)
          )
        ORDER BY updated_at DESC
        LIMIT $3 OFFSET $4
        "#,
    )
    .bind(&kind_filter)
    .bind(&q_filter)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("query error: {e}"),
        )
    })?;

    let items: Vec<MetaDocSummary> = rows
        .into_iter()
        .map(|r| MetaDocSummary {
            id: r.try_get("id").unwrap_or_else(|_| Uuid::nil()),
            kind: r.try_get("kind").unwrap_or_default(),
            title: r.try_get("title").unwrap_or_default(),
            slug: r.try_get("slug").unwrap_or_default(),
            vault_file_path: r.try_get("vault_file_path").unwrap_or_default(),
            tags: r.try_get("tags").unwrap_or_default(),
            auto_generated: r.try_get("auto_generated").unwrap_or(true),
            updated_at: r
                .try_get("updated_at")
                .unwrap_or_else(|_| chrono::Utc::now()),
        })
        .collect();

    let total: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*) FROM nexus_meta_docs
        WHERE ($1 = '' OR kind = $1)
        "#,
    )
    .bind(&kind_filter)
    .fetch_one(&state.db)
    .await
    .unwrap_or(items.len() as i64);

    Ok(Json(json!({
        "items": items,
        "total": total,
        "limit": limit,
        "offset": offset,
    })))
}

/// `GET /api/meta-docs/:id`
pub async fn get_meta_doc(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let row = sqlx::query(
        r#"
        SELECT id, kind, title, slug, body_md, vault_file_path, vault_file_hash,
               source_commit, source_files, tags, auto_generated, created_at, updated_at
        FROM nexus_meta_docs
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("query error: {e}"),
        )
    })?
    .ok_or((StatusCode::NOT_FOUND, "meta-doc non trovata".to_string()))?;

    let kind: String = row.try_get("kind").unwrap_or_default();
    let title: String = row.try_get("title").unwrap_or_default();
    let slug: String = row.try_get("slug").unwrap_or_default();
    let body_md: String = row.try_get("body_md").unwrap_or_default();
    let vault_file_path: String = row.try_get("vault_file_path").unwrap_or_default();
    let vault_file_hash: String = row.try_get("vault_file_hash").unwrap_or_default();
    let source_commit: Option<String> = row.try_get("source_commit").ok();
    let source_files: Vec<String> = row.try_get("source_files").unwrap_or_default();
    let tags: Vec<String> = row.try_get("tags").unwrap_or_default();
    let auto_generated: bool = row.try_get("auto_generated").unwrap_or(true);
    let created_at: chrono::DateTime<chrono::Utc> = row
        .try_get("created_at")
        .unwrap_or_else(|_| chrono::Utc::now());
    let updated_at: chrono::DateTime<chrono::Utc> = row
        .try_get("updated_at")
        .unwrap_or_else(|_| chrono::Utc::now());

    // Carica anche outgoing + incoming links
    let outgoing = sqlx::query(
        "SELECT l.to_doc_id, l.rel_type, l.created_by, l.confidence, d.title, d.slug
         FROM nexus_meta_doc_links l JOIN nexus_meta_docs d ON d.id = l.to_doc_id
         WHERE l.from_doc_id = $1
         ORDER BY l.confidence DESC LIMIT 50",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let incoming = sqlx::query(
        "SELECT l.from_doc_id, l.rel_type, l.created_by, l.confidence, d.title, d.slug
         FROM nexus_meta_doc_links l JOIN nexus_meta_docs d ON d.id = l.from_doc_id
         WHERE l.to_doc_id = $1
         ORDER BY l.confidence DESC LIMIT 50",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let map_links = |rows: Vec<sqlx::postgres::PgRow>| -> Vec<serde_json::Value> {
        rows.into_iter()
            .map(|r| {
                json!({
                    "to_or_from_id": r.try_get::<Uuid, _>(0).ok(),
                    "rel_type": r.try_get::<String, _>(1).unwrap_or_default(),
                    "created_by": r.try_get::<String, _>(2).unwrap_or_default(),
                    "confidence": r.try_get::<f32, _>(3).unwrap_or(0.0),
                    "title": r.try_get::<String, _>(4).unwrap_or_default(),
                    "slug": r.try_get::<String, _>(5).unwrap_or_default(),
                })
            })
            .collect()
    };

    Ok(Json(json!({
        "id": id,
        "kind": kind,
        "title": title,
        "slug": slug,
        "body_md": body_md,
        "vault_file_path": vault_file_path,
        "vault_file_hash": vault_file_hash,
        "source_commit": source_commit,
        "source_files": source_files,
        "tags": tags,
        "auto_generated": auto_generated,
        "created_at": created_at,
        "updated_at": updated_at,
        "outgoing_links": map_links(outgoing),
        "incoming_links": map_links(incoming),
    })))
}

/// `POST /api/meta-docs/refresh-all` — esegue tutti i generator registrati e aggiorna disco+DB.
pub async fn refresh_all_stub(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    use crate::meta_docs::apply::{apply_generated_doc, resolve_vault_root};
    use crate::meta_docs::generators::{all_generators, MetaDocContext};

    let repo_root = std::env::var("NEXUS_REPO_ROOT")
        .unwrap_or_else(|_| "/home/administrator/ideai".to_string());
    let vault_root = resolve_vault_root(&state).await;

    let ctx = MetaDocContext {
        db: &state.db,
        repo_root,
        vault_root: vault_root.clone(),
        commit_sha: None,
        files_changed: Vec::new(),
    };

    let mut all_docs = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for gen in all_generators() {
        if !gen.relevant_for(&ctx.files_changed) {
            continue;
        }
        match gen.generate(&ctx).await {
            Ok(docs) => {
                tracing::info!(
                    generator = %gen.name(),
                    count = docs.len(),
                    "meta-docs generator completato"
                );
                all_docs.extend(docs);
            }
            Err(e) => {
                tracing::warn!(generator = %gen.name(), error = %e, "generator errore");
                errors.push(format!("{}: {}", gen.name(), e));
            }
        }
    }

    let mut applied = 0;
    let mut skipped = 0;
    for doc in &all_docs {
        match apply_generated_doc(&state, &vault_root, doc).await {
            Ok((_, true)) => applied += 1,
            Ok((_, false)) => skipped += 1,
            Err(e) => {
                tracing::warn!(slug = %doc.slug, error = %e, "apply error");
                errors.push(format!("apply {}: {}", doc.slug, e));
            }
        }
    }

    Ok(Json(json!({
        "status": "ok",
        "generated": all_docs.len(),
        "applied": applied,
        "skipped": skipped,
        "errors": errors,
    })))
}

#[derive(Debug, Deserialize)]
pub struct IngestCommitBody {
    pub commit: Option<String>,
    /// Se omesso, lo deduco con `git rev-parse HEAD`.
    pub force: Option<bool>,
}

/// `POST /api/meta-docs/ingest-commit` — chiamato dall'hook lefthook post-commit.
///
/// 1. Legge HEAD via git (o usa `commit` se fornito)
/// 2. INSERT in nexus_meta_doc_changes (ON CONFLICT DO NOTHING)
/// 3. Spawna task in background che esegue i generator rilevanti per i file toccati
pub async fn ingest_commit_stub(
    State(state): State<AppState>,
    body: Option<Json<IngestCommitBody>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    use crate::meta_docs::apply::{apply_generated_doc, resolve_vault_root};
    use crate::meta_docs::generators::{all_generators, MetaDocContext};

    let body = body.map(|Json(b)| b).unwrap_or(IngestCommitBody {
        commit: None,
        force: None,
    });

    let repo_root = std::env::var("NEXUS_REPO_ROOT")
        .unwrap_or_else(|_| "/home/administrator/ideai".to_string());

    // Risolvi commit SHA (HEAD se non fornito)
    let commit_sha = match body.commit {
        Some(c) => c,
        None => {
            let out = tokio::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&repo_root)
                .output()
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("git error: {e}")))?;
            if !out.status.success() {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "git rev-parse HEAD fallito".to_string(),
                ));
            }
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
    };

    // Idempotenza: se gia' processato, esci subito (a meno di force=true)
    let already = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM nexus_meta_doc_changes WHERE commit_sha = $1",
    )
    .bind(&commit_sha)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    if already > 0 && !body.force.unwrap_or(false) {
        return Ok(Json(json!({
            "status": "already_processed",
            "commit": commit_sha,
        })));
    }

    // Estrai commit_msg, author, files_changed
    let msg_out = tokio::process::Command::new("git")
        .args(["log", "-1", "--pretty=%s", &commit_sha])
        .current_dir(&repo_root)
        .output()
        .await
        .ok();
    let commit_msg = msg_out
        .as_ref()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let author_out = tokio::process::Command::new("git")
        .args(["log", "-1", "--pretty=%an", &commit_sha])
        .current_dir(&repo_root)
        .output()
        .await
        .ok();
    let author = author_out
        .as_ref()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    let files_out = tokio::process::Command::new("git")
        .args([
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            &commit_sha,
        ])
        .current_dir(&repo_root)
        .output()
        .await
        .ok();
    let files_changed: Vec<String> = files_out
        .as_ref()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default();

    // INSERT idempotente
    sqlx::query(
        r#"
        INSERT INTO nexus_meta_doc_changes (commit_sha, commit_msg, author, files_changed, significance)
        VALUES ($1, $2, $3, $4, 0.5)
        ON CONFLICT (commit_sha) DO NOTHING
        "#,
    )
    .bind(&commit_sha)
    .bind(&commit_msg)
    .bind(author.as_deref())
    .bind(&files_changed)
    .execute(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("DB insert: {e}")))?;

    // Spawna generator in background (non blocca la risposta)
    let db_clone = state.db.clone();
    let vault_root_clone = resolve_vault_root(&state).await;
    let commit_sha_clone = commit_sha.clone();
    let files_clone = files_changed.clone();
    tokio::spawn(async move {
        let state_for_apply = state.clone();
        let ctx = MetaDocContext {
            db: &db_clone,
            repo_root,
            vault_root: vault_root_clone.clone(),
            commit_sha: Some(commit_sha_clone.clone()),
            files_changed: files_clone.clone(),
        };

        let mut applied = 0;
        for gen in all_generators() {
            if !gen.relevant_for(&files_clone) {
                continue;
            }
            match gen.generate(&ctx).await {
                Ok(docs) => {
                    for doc in &docs {
                        if let Ok((_, true)) =
                            apply_generated_doc(&state_for_apply, &vault_root_clone, doc).await
                        {
                            applied += 1;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(generator = %gen.name(), error = %e, "ingest_commit: generator errore");
                }
            }
        }
        tracing::info!(commit = %commit_sha_clone, applied, "ingest_commit: pipeline completata");
    });

    Ok(Json(json!({
        "status": "accepted",
        "commit": commit_sha,
        "files_changed": files_changed.len(),
        "message": "Generators dispatched in background"
    })))
}

// Note: la registrazione route avviene direttamente in main.rs accanto
// alle altre route del crate (pattern Router<AppState>).

// ── GET /api/meta-docs/export-archive ────────────────────────────────────
//
// Crea un archivio `.tar.gz` della cartella `docs/.nexus-vault/` e lo
// ritorna come download. Usato per scaricare il vault e aprirlo
// localmente in Obsidian (estrarre con tar/7zip/winrar).
// Scelto tar.gz al posto di zip perche' `tar` e' sempre disponibile su
// Linux (zip richiede install separata).

pub async fn export_vault_archive(
    State(state): State<AppState>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    use axum::body::Body;
    use axum::http::header;

    let vault_root = crate::meta_docs::apply::resolve_vault_root(&state).await;

    if !std::path::Path::new(&vault_root).exists() {
        return Err((
            StatusCode::NOT_FOUND,
            format!("vault non trovato: {vault_root}"),
        ));
    }

    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let tmp_archive = format!("/tmp/nexus-vault-{timestamp}.tar.gz");

    let parent = std::path::Path::new(&vault_root).parent().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "parent vault path mancante".to_string(),
    ))?;

    let output = tokio::process::Command::new("tar")
        .args(["-czf", &tmp_archive, ".nexus-vault"])
        .current_dir(parent)
        .output()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("tar exec error: {e}"),
            )
        })?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("tar fallito: {err}"),
        ));
    }

    let bytes = tokio::fs::read(&tmp_archive).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("read archive: {e}"),
        )
    })?;

    let _ = tokio::fs::remove_file(&tmp_archive).await;

    let filename = format!("nexus-meta-vault-{timestamp}.tar.gz");
    let response = axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/gzip")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(Body::from(bytes))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("response build: {e}"),
            )
        })?;

    Ok(response)
}

// ── POST /api/meta-docs/recompute-links ─────────────────────────────────
//
// Per ogni nota: parsa i wikilink `[[slug]]` dal body Markdown e li
// materializza come righe in `nexus_meta_doc_links`. Inoltre aggiunge
// link automatici via similarita' embedding (top-K + soglia).

pub async fn recompute_meta_links(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    use crate::meta_docs::vault::extract_wikilinks;

    // 1. Carica tutte le note: id, slug, body
    let notes = sqlx::query(
        "SELECT id, slug, body_md FROM nexus_meta_docs ORDER BY updated_at DESC LIMIT 5000",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("notes: {e}")))?;

    // Mappa slug -> id per risolvere i wikilink
    let mut slug_to_id: std::collections::HashMap<String, uuid::Uuid> =
        std::collections::HashMap::new();
    let mut all_notes: Vec<(uuid::Uuid, String, String)> = Vec::new();
    for r in &notes {
        let id: uuid::Uuid = r
            .try_get("id")
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("id: {e}")))?;
        let slug: String = r.try_get("slug").unwrap_or_default();
        let body: String = r.try_get("body_md").unwrap_or_default();
        slug_to_id.insert(slug.clone(), id);
        all_notes.push((id, slug, body));
    }

    let mut wikilinks_created = 0usize;
    let mut wikilinks_unresolved = 0usize;

    for (from_id, _slug, body) in &all_notes {
        let links = extract_wikilinks(body);
        for raw in links {
            // Supporta sintassi [[slug#section]] e [[path/slug]] → estrae basename
            let key = raw
                .split('#')
                .next()
                .unwrap_or(&raw)
                .split('/')
                .next_back()
                .unwrap_or(&raw)
                .trim()
                .to_lowercase()
                .replace(' ', "-");

            // Tenta match diretto, poi case-insensitive
            let target_id = slug_to_id
                .get(&key)
                .or_else(|| {
                    slug_to_id
                        .iter()
                        .find(|(k, _)| k.to_lowercase() == key)
                        .map(|(_, v)| v)
                })
                .copied();

            let Some(to_id) = target_id else {
                wikilinks_unresolved += 1;
                continue;
            };
            if to_id == *from_id {
                continue;
            }

            let result = sqlx::query(
                r#"
                INSERT INTO nexus_meta_doc_links
                    (from_doc_id, to_doc_id, rel_type, created_by, confidence)
                VALUES ($1, $2, 'relates', 'auto', 1.0)
                ON CONFLICT (from_doc_id, to_doc_id, rel_type) DO NOTHING
                "#,
            )
            .bind(from_id)
            .bind(to_id)
            .execute(&state.db)
            .await;

            if let Ok(r) = result {
                if r.rows_affected() > 0 {
                    wikilinks_created += 1;
                }
            }
        }
    }

    // ── Fase 2: linking semantico via embedding (top-5 per nota, soglia 0.55) ──
    let mut semantic_created = 0usize;
    let semantic_threshold: f32 = 0.55;
    for (from_id, _slug, body) in &all_notes {
        let embed_text = if body.len() > 2000 {
            &body[..2000]
        } else {
            body
        };
        if embed_text.trim().is_empty() {
            continue;
        }
        let vector = match state.orchestrator.neural.embed_text("", embed_text).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let hits =
            match crate::vector_memory::search_meta_doc_points(&state.db, vector, None, 6).await {
                Ok(h) => h,
                Err(_) => continue,
            };
        for hit in &hits {
            if (hit.score as f32) < semantic_threshold {
                continue;
            }
            let target_id = match hit
                .payload
                .get("doc_id")
                .and_then(|v| v.as_str())
                .and_then(|s| uuid::Uuid::parse_str(s).ok())
            {
                Some(id) if id != *from_id => id,
                _ => continue,
            };
            let result = sqlx::query(
                r#"
                INSERT INTO nexus_meta_doc_links
                    (from_doc_id, to_doc_id, rel_type, created_by, confidence)
                VALUES ($1, $2, 'relates', 'auto', $3)
                ON CONFLICT (from_doc_id, to_doc_id, rel_type) DO UPDATE SET
                    confidence = GREATEST(nexus_meta_doc_links.confidence, EXCLUDED.confidence)
                "#,
            )
            .bind(from_id)
            .bind(target_id)
            .bind(hit.score as f32)
            .execute(&state.db)
            .await;
            if let Ok(r) = result {
                if r.rows_affected() > 0 {
                    semantic_created += 1;
                }
            }
        }
    }

    Ok(Json(json!({
        "ok": true,
        "notes_processed": all_notes.len(),
        "wikilinks_created": wikilinks_created,
        "wikilinks_unresolved": wikilinks_unresolved,
        "semantic_links_created": semantic_created,
    })))
}

// ── GET /api/meta-docs/graph ────────────────────────────────────────────
//
// Ritorna nodi + edge del meta-vault Nexus per Cytoscape.

#[derive(Deserialize)]
pub struct MetaGraphQuery {
    pub kind: Option<String>,
}

pub async fn graph_handler(
    State(state): State<AppState>,
    Query(q): Query<MetaGraphQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let kind_filter = q.kind.unwrap_or_default();

    let nodes = sqlx::query(
        r#"
        SELECT id, kind, title, slug, tags, auto_generated, updated_at
        FROM nexus_meta_docs
        WHERE ($1 = '' OR kind = $1)
        ORDER BY updated_at DESC
        LIMIT 2000
        "#,
    )
    .bind(&kind_filter)
    .fetch_all(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("nodes: {e}")))?;

    let nodes_json: Vec<serde_json::Value> = nodes
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<Uuid, _>("id").ok(),
                "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
                "title": r.try_get::<String, _>("title").unwrap_or_default(),
                "slug": r.try_get::<String, _>("slug").unwrap_or_default(),
                "tags": r.try_get::<Vec<String>, _>("tags").unwrap_or_default(),
                "auto_generated": r.try_get::<bool, _>("auto_generated").unwrap_or(true),
                "updated_at": r.try_get::<chrono::DateTime<chrono::Utc>, _>("updated_at").ok(),
            })
        })
        .collect();

    let edges = sqlx::query(
        r#"
        SELECT id, from_doc_id, to_doc_id, rel_type, created_by, confidence
        FROM nexus_meta_doc_links
        LIMIT 5000
        "#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("edges: {e}")))?;

    let edges_json: Vec<serde_json::Value> = edges
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<Uuid, _>("id").ok(),
                "from": r.try_get::<Uuid, _>("from_doc_id").ok(),
                "to": r.try_get::<Uuid, _>("to_doc_id").ok(),
                "rel_type": r.try_get::<String, _>("rel_type").unwrap_or_default(),
                "created_by": r.try_get::<String, _>("created_by").unwrap_or_default(),
                "confidence": r.try_get::<f32, _>("confidence").unwrap_or(1.0),
            })
        })
        .collect();

    Ok(Json(json!({
        "nodes": nodes_json,
        "edges": edges_json,
        "stats": {
            "nodes_count": nodes_json.len(),
            "edges_count": edges_json.len(),
        }
    })))
}
