// ═══════════════════════════════════════════════════════════════════════════
// wiki/revisions.rs — Lettura/diff/restore delle revisioni unificate.
//
// `record_revision` (mutazione, usata anche da update_doc) vive in
// `storage.rs` per coerenza con la pipeline di UPDATE. Qui restano solo i
// metodi di read e l'operazione composita `restore_revision`.
// ═══════════════════════════════════════════════════════════════════════════

use crate::acl::WikiAcl;
use crate::model::{WikiDoc, WikiRevision};
use crate::storage::record_revision;
use crate::deps::WikiDeps;
use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use uuid::Uuid;

/// Metadata di una revisione (senza body).
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

/// Elenco revisioni applicando ACL: il documento deve essere leggibile.
pub async fn list_revisions(
    state: &WikiDeps,
    acl: &WikiAcl,
    doc_id: Uuid,
) -> Result<Vec<RevisionMeta>> {
    let doc = ensure_doc_readable(state, acl, doc_id).await?;
    let _ = doc; // silenzia warning unused (l'ACL check e' il side-effect utile)

    let rows = sqlx::query_as::<
        _,
        (
            i32,
            String,
            String,
            Option<String>,
            Option<String>,
            i32,
            chrono::DateTime<chrono::Utc>,
        ),
    >(
        r#"
        SELECT version_no, title, source, author, edit_summary,
               length(body_md) AS body_bytes, created_at
        FROM wiki_doc_revisions
        WHERE doc_id = $1
        ORDER BY version_no DESC
        "#,
    )
    .bind(doc_id)
    .fetch_all(&state.db)
    .await
    .context("SELECT wiki_doc_revisions list")?;

    Ok(rows
        .into_iter()
        .map(
            |(version_no, title, source, author, edit_summary, body_bytes, created_at)| {
                RevisionMeta {
                    version_no,
                    title,
                    source,
                    author,
                    edit_summary,
                    body_bytes: body_bytes as i64,
                    created_at,
                }
            },
        )
        .collect())
}

/// Carica una singola revisione completa (con body) applicando ACL.
pub async fn get_revision(
    state: &WikiDeps,
    acl: &WikiAcl,
    doc_id: Uuid,
    version_no: i32,
) -> Result<Option<WikiRevision>> {
    let _doc = ensure_doc_readable(state, acl, doc_id).await?;

    let row: Option<WikiRevision> = sqlx::query_as::<_, WikiRevision>(
        r#"
        SELECT * FROM wiki_doc_revisions
        WHERE doc_id = $1 AND version_no = $2
        "#,
    )
    .bind(doc_id)
    .bind(version_no)
    .fetch_optional(&state.db)
    .await
    .context("SELECT wiki_doc_revisions get")?;

    Ok(row)
}

/// Diff fra due revisioni (ritorna i due body, il rendering del diff vive nel
/// frontend per evitare dipendenze backend).
pub async fn diff(
    state: &WikiDeps,
    acl: &WikiAcl,
    doc_id: Uuid,
    from: i32,
    to: i32,
) -> Result<(WikiRevision, WikiRevision)> {
    let a = get_revision(state, acl, doc_id, from)
        .await?
        .ok_or_else(|| anyhow!("revisione 'from' non trovata"))?;
    let b = get_revision(state, acl, doc_id, to)
        .await?
        .ok_or_else(|| anyhow!("revisione 'to' non trovata"))?;
    Ok((a, b))
}

/// Ripristina il body di una revisione precedente: non distruttivo, crea una
/// nuova revisione con `source='revert'` e aggiorna il body del doc corrente.
pub async fn restore_revision(
    state: &WikiDeps,
    acl: &WikiAcl,
    doc_id: Uuid,
    version: i32,
) -> Result<i32> {
    let doc = ensure_doc_readable(state, acl, doc_id).await?;
    if !acl.can_write(&doc) {
        bail!("permesso negato (restore richiede write)");
    }
    if doc.edit_lock == "frozen" {
        bail!("documento in stato 'frozen': restore vietato");
    }

    let target = get_revision(state, acl, doc_id, version)
        .await?
        .ok_or_else(|| anyhow!("revisione target non trovata"))?;

    // UPDATE del doc corrente: body + bookkeeping edit manuale (il restore
    // viene contato come una modifica manuale per la protezione anti-overwrite).
    sqlx::query(
        r#"
        UPDATE wiki_docs SET
            title           = $1,
            body_md         = $2,
            body_hash       = $3,
            manually_edited = TRUE,
            edited_hash     = $3,
            edited_by       = $4,
            last_edited_at  = NOW(),
            updated_at      = NOW()
        WHERE id = $5
        "#,
    )
    .bind(&target.title)
    .bind(&target.body_md)
    .bind(&target.body_hash)
    .bind(&acl.user_sub)
    .bind(doc_id)
    .execute(&state.db)
    .await
    .context("UPDATE wiki_docs per restore")?;

    let summary = format!("restore della revisione v{version}");
    let new_version = record_revision(
        &state.db,
        doc_id,
        &target.title,
        &target.body_md,
        &target.tags,
        "revert",
        Some(&acl.user_sub),
        Some(&summary),
    )
    .await
    .context("record_revision per restore")?;

    Ok(new_version)
}

/// Helper: ritorna il doc se esiste e l'utente puo' leggerlo; altrimenti error.
async fn ensure_doc_readable(state: &WikiDeps, acl: &WikiAcl, doc_id: Uuid) -> Result<WikiDoc> {
    let doc: WikiDoc = sqlx::query_as::<_, WikiDoc>("SELECT * FROM wiki_docs WHERE id = $1")
        .bind(doc_id)
        .fetch_optional(&state.db)
        .await
        .context("SELECT wiki_docs ensure_readable")?
        .ok_or_else(|| anyhow!("documento non trovato"))?;
    if !acl.can_read(&doc) {
        bail!("permesso negato (lettura)");
    }
    Ok(doc)
}
