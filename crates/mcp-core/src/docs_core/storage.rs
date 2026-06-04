// ═══════════════════════════════════════════════════════════════════════════
// docs_core/storage.rs — CRUD wiki condiviso (meta + progetto).
//
// Fase 3 (storage): unifica la patch manuale dei doc. Le tabelle base
// (nexus_meta_docs, project_knowledge_notes) restano custodi dei dati; qui
// vive la logica di UPDATE + dual-write vault + record_revision, condivisa
// dai due scope. Le pipeline di rigenerazione (generators meta/KB) restano nei
// rispettivi moduli e usano il loro apply specifico (Fase 4: aggancio della
// regola di protezione).
// ═══════════════════════════════════════════════════════════════════════════

use crate::docs_core::revisions::{record_revision, DocScope};
use crate::docs_core::vault::sha256_hex;
use crate::AppState;
use anyhow::{Context, Result};
use sqlx::Row;
use uuid::Uuid;

/// Campi opzionali aggiornabili da una patch wiki. Non includere significa
/// "lascia invariato"; includere `Some` significa aggiornare con quel valore.
#[derive(Debug, Default)]
pub struct DocPatch {
    pub title: Option<String>,
    pub body_md: Option<String>,
    pub tags: Option<Vec<String>>,
    /// Solo per scope progetto (CHECK status su project_knowledge_notes).
    pub status: Option<String>,
    /// Sorgente della revisione che verra' registrata (default "manual").
    /// Usato dal restore per registrare la revisione come "revert".
    pub revision_source: Option<&'static str>,
    /// Edit summary per la revisione (default None).
    pub edit_summary: Option<String>,
}

/// Risultato di una patch: il `version_no` nuovo (o invariato se il body non
/// e' cambiato e quindi non e' stata registrata una nuova revisione).
#[derive(Debug)]
pub struct PatchOutcome {
    pub version_no: i32,
    pub body_changed: bool,
}

/// Aggiorna un documento applicando la patch utente. Effetti:
///   1. UPDATE tabella base (campi forniti + bookkeeping protezione/edit).
///   2. record_revision(source='manual') se il body e' cambiato (idempotente
///      per body_hash: nessuna revisione duplicata).
///   3. Best-effort riscrittura del file vault (se vault_file_path popolato e
///      lo scope sa risolvere il root) — fix coerente con quanto fa apply.rs
///      lato meta; corregge il bug storico per cui patch_note KB NON riscriveva
///      il file.
///
/// Le voci `manually_edited`, `edited_hash`, `last_edited_at`, `edited_by`
/// vengono aggiornate solo se il body e' effettivamente cambiato.
pub async fn update_doc(
    state: &AppState,
    scope: DocScope,
    doc_id: Uuid,
    user_sub: &str,
    patch: DocPatch,
) -> Result<PatchOutcome> {
    // ── 1) Validazione precoce (status valido solo se scope=project) ──────
    if let DocScope::Project(_) = scope {
        if let Some(ref s) = patch.status {
            const ALLOWED: [&str; 4] = ["draft", "active", "archived", "deprecated"];
            if !ALLOWED.contains(&s.as_str()) {
                anyhow::bail!("status non valido (atteso: draft|active|archived|deprecated)");
            }
        }
    }

    // ── 2) Snapshot precedente (necessario per riscrivere il vault) ───────
    let prev = load_doc_snapshot(&state.db, scope, doc_id).await?;
    let body_changed = patch
        .body_md
        .as_deref()
        .map(|new| new != prev.body_md)
        .unwrap_or(false);
    let edited_hash = if body_changed {
        Some(sha256_hex(patch.body_md.as_deref().unwrap_or_default()))
    } else {
        None
    };

    // ── 3) UPDATE atomico via COALESCE (no SET dinamico artigianale) ──────
    // COALESCE($n, col) lascia invariati i campi non passati; le colonne di
    // bookkeeping (manually_edited / edit_lock / edited_*) si auto-aggiornano
    // condizionate su body_changed via $11. Lo scope decide solo se filtrare
    // anche per project_id e se aggiornare status (NULL per scope=meta).
    let status_bind: Option<&str> = match scope {
        DocScope::Project(_) => patch.status.as_deref(),
        DocScope::Meta => None,
    };
    let table = scope.base_table();
    // NB: $4 (status) e' presente nello SET solo nello statement project (la
    // colonna `status` non esiste su nexus_meta_docs).
    let sql = match scope {
        DocScope::Meta => format!(
            r#"
            UPDATE {table} SET
                title           = COALESCE($1, title),
                body_md         = COALESCE($2, body_md),
                tags            = COALESCE($3, tags),
                manually_edited = CASE WHEN $5 THEN TRUE ELSE manually_edited END,
                edit_lock       = CASE
                                    WHEN edit_lock = 'frozen' THEN 'frozen'
                                    WHEN $5 THEN 'protected'
                                    ELSE edit_lock
                                  END,
                edited_hash     = COALESCE($6, edited_hash),
                edited_by       = CASE WHEN $5 THEN $7 ELSE edited_by END,
                last_edited_at  = CASE WHEN $5 THEN NOW() ELSE last_edited_at END,
                updated_at      = NOW()
            WHERE id = $8
            "#
        ),
        DocScope::Project(_) => format!(
            r#"
            UPDATE {table} SET
                title           = COALESCE($1, title),
                body_md         = COALESCE($2, body_md),
                tags            = COALESCE($3, tags),
                status          = COALESCE($4, status),
                manually_edited = CASE WHEN $5 THEN TRUE ELSE manually_edited END,
                edit_lock       = CASE
                                    WHEN edit_lock = 'frozen' THEN 'frozen'
                                    WHEN $5 THEN 'protected'
                                    ELSE edit_lock
                                  END,
                edited_hash     = COALESCE($6, edited_hash),
                edited_by       = CASE WHEN $5 THEN $7 ELSE edited_by END,
                last_edited_at  = CASE WHEN $5 THEN NOW() ELSE last_edited_at END,
                updated_at      = NOW()
            WHERE id = $8 AND project_id = $9
            "#
        ),
    };

    let mut q = sqlx::query(&sql)
        .bind(patch.title.as_deref())
        .bind(patch.body_md.as_deref())
        .bind(patch.tags.as_deref())
        .bind(status_bind)
        .bind(body_changed)
        .bind(edited_hash.as_deref())
        .bind(user_sub)
        .bind(doc_id);
    if let DocScope::Project(pid) = scope {
        q = q.bind(pid);
    }
    let res = q.execute(&state.db).await.context("UPDATE doc")?;
    if res.rows_affected() == 0 {
        anyhow::bail!("documento non trovato");
    }

    // ── 4) Riscrittura vault best-effort (solo se body cambiato) ──────────
    if body_changed {
        let new_title = patch.title.as_deref().unwrap_or(&prev.title);
        let new_body = patch.body_md.as_deref().unwrap_or(&prev.body_md);
        let new_tags = patch.tags.clone().unwrap_or_else(|| prev.tags.clone());
        if let Err(e) =
            rewrite_vault_file(state, scope, doc_id, &prev, new_title, new_body, &new_tags).await
        {
            tracing::debug!(doc_id = %doc_id, error = %e, "rewrite vault best-effort fallita");
        }
    }

    // ── 5) Registra revisione (dedup automatico per body_hash, vedi CTE) ──
    let new_title = patch.title.as_deref().unwrap_or(&prev.title);
    let new_body = patch.body_md.as_deref().unwrap_or(&prev.body_md);
    let new_tags = patch.tags.clone().unwrap_or_else(|| prev.tags.clone());
    let source = patch.revision_source.unwrap_or("manual");
    let version_no = record_revision(
        &state.db,
        scope,
        doc_id,
        new_title,
        new_body,
        &new_tags,
        source,
        Some(user_sub),
        patch.edit_summary.as_deref(),
    )
    .await?;

    Ok(PatchOutcome {
        version_no,
        body_changed,
    })
}

/// Snapshot dello stato corrente del doc, sufficiente per riserializzare il
/// vault e calcolare `body_changed`. Una sola query SELECT scope-specifica.
struct DocSnapshot {
    title: String,
    body_md: String,
    tags: Vec<String>,
    // Campi extra usati da `rewrite_vault_file` (scope-specifici).
    vault_file_path: Option<String>,
    kind: Option<String>,
    slug: Option<String>,
    source_commit: Option<String>,
    source_files: Vec<String>,
    intent: Option<String>,
    status: Option<String>,
    source_message_id: Option<Uuid>,
    source_run_id: Option<Uuid>,
    file_paths: Vec<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn load_doc_snapshot(
    db: &sqlx::PgPool,
    scope: DocScope,
    doc_id: Uuid,
) -> Result<DocSnapshot> {
    let row = match scope {
        DocScope::Meta => sqlx::query(
            r#"
            SELECT title, body_md, tags, kind, slug, vault_file_path,
                   source_commit, source_files, created_at
            FROM nexus_meta_docs WHERE id = $1
            "#,
        )
        .bind(doc_id)
        .fetch_optional(db)
        .await
        .context("SELECT nexus_meta_docs")?,
        DocScope::Project(pid) => sqlx::query(
            r#"
            SELECT title, body_md, tags, status, intent, vault_file_path,
                   source_message_id, source_run_id, file_paths, created_at
            FROM project_knowledge_notes WHERE id = $1 AND project_id = $2
            "#,
        )
        .bind(doc_id)
        .bind(pid)
        .fetch_optional(db)
        .await
        .context("SELECT project_knowledge_notes")?,
    }
    .ok_or_else(|| anyhow::anyhow!("documento non trovato"))?;

    Ok(DocSnapshot {
        title: row.try_get("title").unwrap_or_default(),
        body_md: row.try_get("body_md").unwrap_or_default(),
        tags: row.try_get("tags").unwrap_or_default(),
        vault_file_path: row.try_get("vault_file_path").ok(),
        kind: row.try_get("kind").ok(),
        slug: row.try_get("slug").ok(),
        source_commit: row.try_get("source_commit").ok(),
        source_files: row.try_get("source_files").unwrap_or_default(),
        intent: row.try_get("intent").ok(),
        status: row.try_get("status").ok(),
        source_message_id: row.try_get("source_message_id").ok(),
        source_run_id: row.try_get("source_run_id").ok(),
        file_paths: row.try_get("file_paths").unwrap_or_default(),
        created_at: row
            .try_get("created_at")
            .unwrap_or_else(|_| chrono::Utc::now()),
    })
}

/// Riscrive il file vault del documento per riflettere body/title/tags nuovi.
/// Best-effort: se non si riesce a risolvere il root o a serializzare,
/// si logga e si ritorna Ok (la DB resta la fonte autorevole). Aggiorna
/// `vault_file_hash` con il nuovo hash per la loop-detection del watcher.
async fn rewrite_vault_file(
    state: &AppState,
    scope: DocScope,
    doc_id: Uuid,
    prev: &DocSnapshot,
    new_title: &str,
    new_body: &str,
    new_tags: &[String],
) -> Result<()> {
    let Some(rel_path) = prev.vault_file_path.as_deref().filter(|s| !s.is_empty()) else {
        return Ok(());
    };

    // 1) Serializza il nuovo body completo (frontmatter + markdown).
    let now = chrono::Utc::now();
    let body_full = match scope {
        DocScope::Meta => crate::meta_docs::vault::serialize_meta_doc(
            doc_id,
            prev.kind.as_deref().unwrap_or("other"),
            new_title,
            prev.slug.as_deref().unwrap_or(""),
            new_tags,
            prev.source_commit.as_deref(),
            &prev.source_files,
            false, // edit manuale -> auto_generated=false a livello frontmatter
            &prev.created_at,
            &now,
            new_body,
            &[] as &[crate::meta_docs::vault::VaultMetaLink],
        ),
        DocScope::Project(pid) => crate::knowledge::vault::serialize_note(
            doc_id,
            pid,
            prev.source_message_id,
            prev.source_run_id,
            prev.intent.as_deref(),
            prev.status.as_deref().unwrap_or("active"),
            new_tags,
            &prev.file_paths,
            &prev.created_at,
            &now,
            new_title,
            new_body,
            &[] as &[crate::knowledge::vault::VaultNoteLink],
        ),
    };

    // 2) Risolvi il root del vault (meta = fisso da settings; project = DB).
    let vault_root = match scope {
        DocScope::Meta => Some(crate::meta_docs::apply::resolve_vault_root(state).await),
        DocScope::Project(pid) => sqlx::query_scalar::<_, String>(
            "SELECT repository_root_path FROM projects WHERE id = $1",
        )
        .bind(pid)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten(),
    };

    // 3) Scrivi su disco (best-effort) e aggiorna vault_file_hash nel DB.
    if let Some(root) = vault_root {
        let full_path = format!("{}/{}", root.trim_end_matches('/'), rel_path);
        if let Some(parent) = std::path::Path::new(&full_path).parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let _ = tokio::fs::write(&full_path, &body_full).await;
    }
    let new_hash = sha256_hex(&body_full);
    let bump_sql = format!(
        "UPDATE {} SET vault_file_hash = $1 WHERE id = $2",
        scope.base_table()
    );
    let _ = sqlx::query(&bump_sql)
        .bind(&new_hash)
        .bind(doc_id)
        .execute(&state.db)
        .await;
    Ok(())
}
