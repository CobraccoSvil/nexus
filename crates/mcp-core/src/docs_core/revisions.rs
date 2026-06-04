// ═══════════════════════════════════════════════════════════════════════════
// docs_core/revisions.rs — Versioning condiviso dei doc wiki (meta + progetto).
//
// Storage = full snapshot del body in `wiki_doc_revisions` (vedi mig 0282).
// `record_revision` e' riusato da tutti i write (generatori, patch manuale,
// watcher, restore): dedup per body_hash, bump di current_version sulla tabella
// base corrispondente allo scope. Gli handler HTTP qui esposti coprono lo scope
// meta; gli handler progetto riusano le stesse funzioni core (fase storage).
// ═══════════════════════════════════════════════════════════════════════════

use crate::docs_core::vault::sha256_hex;
use crate::AppState;
use anyhow::{Context, Result};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

/// Scope di un documento wiki. `Meta` = documentazione del meta-progetto Nexus
/// (project_id NULL); `Project` = Knowledge Base di un progetto registrato.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocScope {
    Meta,
    Project(Uuid),
}

impl DocScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            DocScope::Meta => "meta",
            DocScope::Project(_) => "project",
        }
    }
    pub fn project_id(&self) -> Option<Uuid> {
        match self {
            DocScope::Meta => None,
            DocScope::Project(id) => Some(*id),
        }
    }
    /// Nome della tabella base che ospita il documento (per il bump di
    /// current_version). Le due tabelle condividono le colonne di versioning
    /// aggiunte dalla mig 0282.
    pub fn base_table(&self) -> &'static str {
        match self {
            DocScope::Meta => "nexus_meta_docs",
            DocScope::Project(_) => "project_knowledge_notes",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RevisionMeta {
    pub version_no: i32,
    pub title: String,
    pub source: String,
    pub author: Option<String>,
    pub edit_summary: Option<String>,
    pub body_bytes: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
pub struct RevisionFull {
    pub version_no: i32,
    pub title: String,
    pub body_md: String,
    pub tags: Vec<String>,
    pub source: String,
    pub author: Option<String>,
    pub edit_summary: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Registra una nuova revisione del documento. Idempotente per contenuto:
/// se l'ultima revisione ha lo stesso `body_hash`, non inserisce nulla e
/// ritorna la versione corrente (dedup, evita storm dei watcher).
///
/// Implementazione single-roundtrip via CTE: una unica query atomica esegue
/// (1) lookup ultima versione+hash via indice `idx_wiki_rev_doc_latest`,
/// (2) INSERT condizionale se hash diverso, (3) UPDATE current_version sulla
/// tabella base solo se l'INSERT e' avvenuto. Niente race window tra SELECT max
/// e INSERT (l'unique `(scope, doc_id, version_no)` resta come safety net).
#[allow(clippy::too_many_arguments)]
pub async fn record_revision(
    db: &sqlx::PgPool,
    scope: DocScope,
    doc_id: Uuid,
    title: &str,
    body_md: &str,
    tags: &[String],
    source: &str,
    author: Option<&str>,
    edit_summary: Option<&str>,
) -> Result<i32> {
    let body_hash = sha256_hex(body_md);
    let base_table = scope.base_table();

    // Pipeline CTE atomica: last → ins (condizionale) → bump_base → SELECT version
    // base_table interpolato (input fidato: `DocScope::base_table` ritorna
    // `&'static str` letterali "nexus_meta_docs" / "project_knowledge_notes").
    let sql = format!(
        r#"
        WITH last AS (
            SELECT version_no, body_hash
            FROM wiki_doc_revisions
            WHERE scope = $1 AND doc_id = $2
            ORDER BY version_no DESC
            LIMIT 1
        ),
        ins AS (
            INSERT INTO wiki_doc_revisions
                (scope, doc_id, project_id, version_no, title, body_md,
                 body_hash, tags, source, author, edit_summary)
            SELECT $1, $2, $3,
                   COALESCE((SELECT version_no FROM last), 0) + 1,
                   $4, $5, $6, $7, $8, $9, $10
            WHERE NOT EXISTS (
                SELECT 1 FROM last WHERE body_hash = $6
            )
            RETURNING version_no
        ),
        bump_base AS (
            UPDATE {base_table}
               SET current_version = (SELECT version_no FROM ins),
                   updated_at = NOW()
             WHERE id = $2 AND EXISTS (SELECT 1 FROM ins)
            RETURNING 1
        )
        SELECT COALESCE(
            (SELECT version_no FROM ins),
            (SELECT version_no FROM last),
            0
        ) AS version_no
        "#
    );

    let version: i32 = sqlx::query_scalar(&sql)
        .bind(scope.as_str())
        .bind(doc_id)
        .bind(scope.project_id())
        .bind(title)
        .bind(body_md)
        .bind(&body_hash)
        .bind(tags)
        .bind(source)
        .bind(author)
        .bind(edit_summary)
        .fetch_one(db)
        .await
        .context("record_revision (CTE)")?;

    Ok(version)
}

/// Elenco revisioni (senza body) ordinate dalla piu' recente.
pub async fn list_revisions(
    db: &sqlx::PgPool,
    scope: DocScope,
    doc_id: Uuid,
) -> Result<Vec<RevisionMeta>> {
    let rows = sqlx::query(
        r#"
        SELECT version_no, title, source, author, edit_summary,
               length(body_md) AS body_bytes, created_at
        FROM wiki_doc_revisions
        WHERE scope = $1 AND doc_id = $2
        ORDER BY version_no DESC
        "#,
    )
    .bind(scope.as_str())
    .bind(doc_id)
    .fetch_all(db)
    .await
    .context("list_revisions")?;

    Ok(rows
        .into_iter()
        .map(|r| RevisionMeta {
            version_no: r.try_get("version_no").unwrap_or(0),
            title: r.try_get("title").unwrap_or_default(),
            source: r.try_get("source").unwrap_or_default(),
            author: r.try_get("author").ok(),
            edit_summary: r.try_get("edit_summary").ok(),
            body_bytes: r.try_get::<i32, _>("body_bytes").unwrap_or(0) as i64,
            created_at: r.try_get("created_at").unwrap_or_else(|_| chrono::Utc::now()),
        })
        .collect())
}

/// Carica una singola revisione completa (con body).
pub async fn get_revision(
    db: &sqlx::PgPool,
    scope: DocScope,
    doc_id: Uuid,
    version_no: i32,
) -> Result<Option<RevisionFull>> {
    let row = sqlx::query(
        r#"
        SELECT version_no, title, body_md, tags, source, author, edit_summary, created_at
        FROM wiki_doc_revisions
        WHERE scope = $1 AND doc_id = $2 AND version_no = $3
        "#,
    )
    .bind(scope.as_str())
    .bind(doc_id)
    .bind(version_no)
    .fetch_optional(db)
    .await
    .context("get_revision")?;

    Ok(row.map(|r| RevisionFull {
        version_no: r.try_get("version_no").unwrap_or(0),
        title: r.try_get("title").unwrap_or_default(),
        body_md: r.try_get("body_md").unwrap_or_default(),
        tags: r.try_get("tags").unwrap_or_default(),
        source: r.try_get("source").unwrap_or_default(),
        author: r.try_get("author").ok(),
        edit_summary: r.try_get("edit_summary").ok(),
        created_at: r.try_get("created_at").unwrap_or_else(|_| chrono::Utc::now()),
    }))
}

// ───────────────────────── Handler HTTP (scope meta) ─────────────────────────

/// `GET /api/meta-docs/:id/revisions`
pub async fn meta_list_revisions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let items = list_revisions(&state.db, DocScope::Meta, id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")))?;
    Ok(Json(json!({ "items": items, "total": items.len() })))
}

/// `GET /api/meta-docs/:id/revisions/:version`
pub async fn meta_get_revision(
    State(state): State<AppState>,
    Path((id, version)): Path<(Uuid, i32)>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let rev = get_revision(&state.db, DocScope::Meta, id, version)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")))?;
    match rev {
        Some(r) => Ok(Json(serde_json::to_value(r).unwrap_or_else(|_| json!({})))),
        None => Err((StatusCode::NOT_FOUND, "revisione non trovata".to_string())),
    }
}

#[derive(Debug, Deserialize)]
pub struct DiffQuery {
    pub from: i32,
    pub to: i32,
}

/// `GET /api/meta-docs/:id/diff?from=&to=` — ritorna i due body (il rendering
/// del diff e' a carico del frontend, zero dipendenze backend).
pub async fn meta_diff(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<DiffQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let from = get_revision(&state.db, DocScope::Meta, id, q.from)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")))?;
    let to = get_revision(&state.db, DocScope::Meta, id, q.to)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")))?;
    match (from, to) {
        (Some(a), Some(b)) => Ok(Json(json!({ "from": a, "to": b }))),
        _ => Err((StatusCode::NOT_FOUND, "revisione non trovata".to_string())),
    }
}

#[derive(Debug, Deserialize)]
pub struct RestoreBody {
    pub version: i32,
}

/// `POST /api/meta-docs/:id/restore { version }` — ripristina il body di una
/// revisione precedente (non distruttivo: crea una nuova revisione source=revert)
/// e riscrive il file vault.
pub async fn meta_restore(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Extension(claims): Extension<crate::auth::Claims>,
    Json(body): Json<RestoreBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let target = get_revision(&state.db, DocScope::Meta, id, body.version)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")))?
        .ok_or((StatusCode::NOT_FOUND, "revisione non trovata".to_string()))?;

    // Campi correnti del doc per la riserializzazione del vault.
    let row = sqlx::query(
        r#"
        SELECT kind, slug, tags, source_commit, source_files, vault_file_path, created_at
        FROM nexus_meta_docs WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")))?
    .ok_or((StatusCode::NOT_FOUND, "documento non trovato".to_string()))?;

    let kind: String = row.try_get("kind").unwrap_or_default();
    let slug: String = row.try_get("slug").unwrap_or_default();
    let tags: Vec<String> = row.try_get("tags").unwrap_or_default();
    let source_commit: Option<String> = row.try_get("source_commit").ok();
    let source_files: Vec<String> = row.try_get("source_files").unwrap_or_default();
    let vault_file_path: String = row.try_get("vault_file_path").unwrap_or_default();
    let created_at: chrono::DateTime<chrono::Utc> =
        row.try_get("created_at").unwrap_or_else(|_| chrono::Utc::now());
    let now = chrono::Utc::now();

    let body_full = crate::meta_docs::vault::serialize_meta_doc(
        id,
        &kind,
        &target.title,
        &slug,
        &tags,
        source_commit.as_deref(),
        &source_files,
        true,
        &created_at,
        &now,
        &target.body_md,
        &[] as &[crate::meta_docs::vault::VaultMetaLink],
    );
    let new_hash = sha256_hex(&body_full);

    // Riscrivi il file vault (best-effort: il DB resta la fonte autorevole).
    let vault_root = crate::meta_docs::apply::resolve_vault_root(&state).await;
    let full_path = format!("{}/{}", vault_root.trim_end_matches('/'), vault_file_path);
    if let Some(parent) = std::path::Path::new(&full_path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let _ = tokio::fs::write(&full_path, &body_full).await;

    // Aggiorna la tabella base: body + hash + marca come modifica manuale.
    sqlx::query(
        r#"
        UPDATE nexus_meta_docs
        SET title = $1, body_md = $2, vault_file_hash = $3,
            manually_edited = TRUE, edited_hash = $3, edited_by = $4,
            last_edited_at = NOW(), updated_at = NOW()
        WHERE id = $5
        "#,
    )
    .bind(&target.title)
    .bind(&target.body_md)
    .bind(&new_hash)
    .bind(&claims.sub)
    .bind(id)
    .execute(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")))?;

    let summary = format!("restore della revisione v{}", body.version);
    let new_version = record_revision(
        &state.db,
        DocScope::Meta,
        id,
        &target.title,
        &target.body_md,
        &tags,
        "revert",
        Some(&claims.sub),
        Some(&summary),
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")))?;

    Ok(Json(json!({
        "ok": true,
        "restored_from": body.version,
        "version": new_version,
    })))
}
