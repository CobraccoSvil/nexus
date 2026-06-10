// ═══════════════════════════════════════════════════════════════════════════
// wiki/storage.rs — CRUD su `wiki_docs` con revisioni atomiche.
//
// Tutte le funzioni qui presenti rispettano l'ACL: i chiamanti devono passare
// `WikiAcl` e queste funzioni applicano automaticamente il filtro WHERE
// (`scope_clause`) o ritornano errore se la write non e' consentita.
//
// Lo schema sorgente e' la migrazione 0295 (vedi ADR 0017 v2). Niente fallback
// hardcoded di nomi colonna: se la mig non e' applicata, le query falliscono
// a runtime (regola H: il problema e' "applica la mig", non "patcha il codice").
// ═══════════════════════════════════════════════════════════════════════════

use crate::wiki::acl::WikiAcl;
use crate::wiki::model::{WikiDoc, WikiDocPatch, WikiScope};
use crate::wiki::vault::{sha256_hex, slugify};
use crate::AppState;
use anyhow::{anyhow, bail, Context, Result};
use uuid::Uuid;

/// Input minimo per creare un nuovo documento. Il caller e' responsabile di
/// fornire `scope` + (eventuale) `project_id` coerenti: il CHECK SQL
/// `scope_project_consistency` rifiuta input incoerenti.
#[derive(Debug)]
pub struct WikiDocCreate {
    pub scope: WikiScope,
    pub project_id: Option<Uuid>,
    pub kind: String,
    pub title: String,
    /// Se `None`, derivato da `slugify(title)`.
    pub slug: Option<String>,
    pub body_md: String,
    pub tags: Vec<String>,
    pub intent: Option<String>,
    pub public_read: bool,
    /// Se l'utente passa esplicitamente un vault_file_path (es. import da
    /// vault esistente); altrimenti `build_vault_path` lo deriva da kind/slug.
    pub vault_file_path: Option<String>,
}

/// Outcome di una patch.
#[derive(Debug)]
pub struct PatchOutcome {
    pub version_no: i32,
    pub body_changed: bool,
}

/// Crea un nuovo documento applicando l'ACL: scope=meta richiede admin,
/// scope=project richiede membership.
pub async fn create_doc(state: &AppState, acl: &WikiAcl, input: WikiDocCreate) -> Result<WikiDoc> {
    // ── ACL preventiva ───────────────────────────────────────────────────
    match input.scope {
        WikiScope::Meta => {
            if !acl.is_admin {
                bail!("solo admin puo' creare meta-doc");
            }
            if input.project_id.is_some() {
                bail!("scope=meta non ammette project_id");
            }
        }
        WikiScope::Project => {
            let pid = input
                .project_id
                .ok_or_else(|| anyhow!("scope=project richiede project_id"))?;
            if !acl.is_admin && !acl.project_ids.contains(&pid) {
                bail!("utente non membro del progetto target");
            }
        }
    }

    // ── Derivazione campi ────────────────────────────────────────────────
    let slug = input
        .slug
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| slugify(&input.title));
    if slug.is_empty() {
        bail!("slug vuoto (title non genera slug valido)");
    }
    let body_hash = sha256_hex(&input.body_md);

    // ── INSERT (lo schema applica i CHECK scope_project_consistency e
    //    public_read_meta_only). UNIQUE su (scope, project_id, slug) sale come
    //    errore in caso di duplicato. ──
    let row: WikiDoc = sqlx::query_as::<_, WikiDoc>(
        r#"
        INSERT INTO wiki_docs (
            scope, project_id, slug, title, body_md, body_hash,
            kind, intent, tags, vault_file_path,
            edit_lock, protected_sections, manually_edited,
            generated_hash, edited_hash,
            current_version, auto_generated, public_read
        ) VALUES (
            $1, $2, $3, $4, $5, $6,
            $7, $8, $9, $10,
            'none', '{}', FALSE,
            NULL, NULL,
            1, FALSE, $11
        )
        RETURNING *
        "#,
    )
    .bind(input.scope.as_str())
    .bind(input.project_id)
    .bind(&slug)
    .bind(&input.title)
    .bind(&input.body_md)
    .bind(&body_hash)
    .bind(&input.kind)
    .bind(input.intent.as_deref())
    .bind(&input.tags)
    .bind(input.vault_file_path.as_deref())
    .bind(input.public_read)
    .fetch_one(&state.db)
    .await
    .context("INSERT wiki_docs")?;

    // ── Prima revisione (source=manual, author=user_sub) ──────────────────
    let _ = record_revision(
        &state.db,
        row.id,
        &row.title,
        &row.body_md,
        &row.tags,
        "manual",
        Some(&acl.user_sub),
        None,
    )
    .await
    .context("record_revision per create_doc")?;

    Ok(row)
}

/// Carica un singolo documento applicando l'ACL. Ritorna `None` se inesistente
/// o se l'utente non ha permessi di lettura.
pub async fn get_doc(state: &AppState, acl: &WikiAcl, doc_id: Uuid) -> Result<Option<WikiDoc>> {
    let row: Option<WikiDoc> =
        sqlx::query_as::<_, WikiDoc>("SELECT * FROM wiki_docs WHERE id = $1")
            .bind(doc_id)
            .fetch_optional(&state.db)
            .await
            .context("SELECT wiki_docs by id")?;

    match row {
        Some(doc) if acl.can_read(&doc) => Ok(Some(doc)),
        _ => Ok(None),
    }
}

/// Parametri di lista.
#[derive(Debug, Default)]
pub struct WikiListQuery {
    pub scope: Option<WikiScope>,
    pub project_id: Option<Uuid>,
    pub kind: Option<String>,
    pub q: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

/// Elenca documenti applicando ACL + filtri. Ritorna `(items, total)`.
pub async fn list_docs(
    state: &AppState,
    acl: &WikiAcl,
    query: WikiListQuery,
) -> Result<(Vec<WikiDoc>, i64)> {
    let limit = query.limit.clamp(1, 200);
    let offset = query.offset.max(0);

    // Costruzione clausole WHERE incrementali. I parametri sono numerati a
    // partire da $1 in ordine di bind.
    let mut where_parts: Vec<String> = Vec::new();
    let (acl_clause, acl_projects) = acl.scope_clause(1);
    where_parts.push(acl_clause);
    let acl_param_used = !acl_projects.is_empty();

    // Indice del prossimo parametro libero ($2 se ACL ha bind, $1 altrimenti).
    let mut next_idx = if acl_param_used { 2 } else { 1 };

    if let Some(s) = query.scope.as_ref() {
        where_parts.push(format!("wiki_docs.scope = ${next_idx}"));
        next_idx += 1;
        // Salveremo s.as_str() nel bind alla fine.
        let _ = s; // marker
    }
    if query.project_id.is_some() {
        where_parts.push(format!("wiki_docs.project_id = ${next_idx}"));
        next_idx += 1;
    }
    if query.kind.as_ref().is_some_and(|k| !k.is_empty()) {
        where_parts.push(format!("wiki_docs.kind = ${next_idx}"));
        next_idx += 1;
    }
    if query.q.as_ref().is_some_and(|q| !q.is_empty()) {
        // Match testuale semplice: ILIKE su title + body. Per ora niente FTS.
        where_parts.push(format!(
            "(wiki_docs.title ILIKE ${next_idx} OR wiki_docs.body_md ILIKE ${next_idx})"
        ));
        next_idx += 1;
    }
    let where_clause = where_parts.join(" AND ");

    let sql_items = format!(
        "SELECT * FROM wiki_docs WHERE {where_clause} \
         ORDER BY updated_at DESC LIMIT ${} OFFSET ${}",
        next_idx,
        next_idx + 1
    );
    let sql_count = format!("SELECT COUNT(*) FROM wiki_docs WHERE {where_clause}");

    // Bind comune (ACL + filtri).
    let mut q_items = sqlx::query_as::<_, WikiDoc>(&sql_items);
    let mut q_count = sqlx::query_scalar::<_, i64>(&sql_count);
    if acl_param_used {
        q_items = q_items.bind(acl_projects.clone());
        q_count = q_count.bind(acl_projects.clone());
    }
    if let Some(s) = query.scope.as_ref() {
        q_items = q_items.bind(s.as_str());
        q_count = q_count.bind(s.as_str());
    }
    if let Some(pid) = query.project_id {
        q_items = q_items.bind(pid);
        q_count = q_count.bind(pid);
    }
    if let Some(k) = query.kind.as_ref().filter(|k| !k.is_empty()) {
        q_items = q_items.bind(k.clone());
        q_count = q_count.bind(k.clone());
    }
    if let Some(qstr) = query.q.as_ref().filter(|q| !q.is_empty()) {
        let pattern = format!("%{qstr}%");
        q_items = q_items.bind(pattern.clone());
        q_count = q_count.bind(pattern);
    }

    let items = q_items
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db)
        .await
        .context("SELECT wiki_docs list")?;
    let total = q_count
        .fetch_one(&state.db)
        .await
        .context("COUNT wiki_docs list")?;

    Ok((items, total))
}

/// Aggiorna un documento applicando ACL + patch parziale. Effetti:
///   1. ACL check (`can_write`).
///   2. UPDATE atomico via COALESCE (campi None lasciano invariato).
///   3. Se body cambiato: registra revisione (`record_revision`).
///   4. Best-effort: riscrittura del file vault (TODO worker dedicato in F3).
pub async fn update_doc(
    state: &AppState,
    acl: &WikiAcl,
    doc_id: Uuid,
    patch: WikiDocPatch,
) -> Result<PatchOutcome> {
    // ── 1) Fetch corrente per ACL + diff body ─────────────────────────────
    let prev: WikiDoc = sqlx::query_as::<_, WikiDoc>("SELECT * FROM wiki_docs WHERE id = $1")
        .bind(doc_id)
        .fetch_optional(&state.db)
        .await
        .context("SELECT wiki_docs per update_doc")?
        .ok_or_else(|| anyhow!("documento non trovato"))?;

    if !acl.can_write(&prev) {
        bail!("permesso negato (utente non autorizzato a modificare questo documento)");
    }
    if prev.edit_lock == "frozen" {
        bail!("documento in stato 'frozen': modifica vietata");
    }

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

    // ── 2) UPDATE atomico con COALESCE ────────────────────────────────────
    sqlx::query(
        r#"
        UPDATE wiki_docs SET
            title           = COALESCE($1, title),
            body_md         = COALESCE($2, body_md),
            body_hash       = COALESCE($3, body_hash),
            tags            = COALESCE($4, tags),
            intent          = COALESCE($5, intent),
            manually_edited = CASE WHEN $6 THEN TRUE ELSE manually_edited END,
            edit_lock       = CASE
                                WHEN edit_lock = 'frozen' THEN 'frozen'
                                WHEN $6 THEN 'protected'
                                ELSE edit_lock
                              END,
            edited_hash     = COALESCE($7, edited_hash),
            edited_by       = CASE WHEN $6 THEN $8 ELSE edited_by END,
            last_edited_at  = CASE WHEN $6 THEN NOW() ELSE last_edited_at END,
            updated_at      = NOW()
        WHERE id = $9
        "#,
    )
    .bind(patch.title.as_deref())
    .bind(patch.body_md.as_deref())
    .bind(edited_hash.as_deref())
    .bind(patch.tags.as_deref())
    .bind(patch.intent.as_deref())
    .bind(body_changed)
    .bind(edited_hash.as_deref())
    .bind(&acl.user_sub)
    .bind(doc_id)
    .execute(&state.db)
    .await
    .context("UPDATE wiki_docs")?;

    // ── 3) Registra revisione (dedup automatico per body_hash) ────────────
    let new_title = patch.title.as_deref().unwrap_or(&prev.title);
    let new_body = patch.body_md.as_deref().unwrap_or(&prev.body_md);
    let new_tags = patch.tags.clone().unwrap_or_else(|| prev.tags.clone());
    let source = patch.revision_source.as_deref().unwrap_or("manual");
    let version_no = record_revision(
        &state.db,
        doc_id,
        new_title,
        new_body,
        &new_tags,
        source,
        Some(&acl.user_sub),
        patch.edit_summary.as_deref(),
    )
    .await
    .context("record_revision per update_doc")?;

    Ok(PatchOutcome {
        version_no,
        body_changed,
    })
}

/// Elimina un documento (cascade su revisioni, link, triple via FK). ACL: same
/// regola di `can_write` (admin per scope=meta, membro per scope=project).
pub async fn delete_doc(state: &AppState, acl: &WikiAcl, doc_id: Uuid) -> Result<()> {
    let prev: WikiDoc = sqlx::query_as::<_, WikiDoc>("SELECT * FROM wiki_docs WHERE id = $1")
        .bind(doc_id)
        .fetch_optional(&state.db)
        .await
        .context("SELECT wiki_docs per delete_doc")?
        .ok_or_else(|| anyhow!("documento non trovato"))?;
    if !acl.can_write(&prev) {
        bail!("permesso negato");
    }
    if prev.edit_lock == "frozen" {
        bail!("documento in stato 'frozen': cancellazione vietata");
    }
    sqlx::query("DELETE FROM wiki_docs WHERE id = $1")
        .bind(doc_id)
        .execute(&state.db)
        .await
        .context("DELETE wiki_docs")?;
    Ok(())
}

/// Registra una revisione nella tabella `wiki_doc_revisions`. Idempotente per
/// body_hash: se l'ultima revisione ha lo stesso hash, ritorna la versione
/// esistente senza INSERT (dedup, evita storm dei watcher futuri).
///
/// CTE atomica: lookup ultima versione+hash, INSERT condizionale, UPDATE
/// `current_version` sulla riga di `wiki_docs` se l'INSERT e' avvenuto.
#[allow(clippy::too_many_arguments)]
pub async fn record_revision(
    db: &sqlx::PgPool,
    doc_id: Uuid,
    title: &str,
    body_md: &str,
    tags: &[String],
    source: &str,
    author: Option<&str>,
    edit_summary: Option<&str>,
) -> Result<i32> {
    let body_hash = sha256_hex(body_md);
    let sql = r#"
        WITH last AS (
            SELECT version_no, body_hash
            FROM wiki_doc_revisions
            WHERE doc_id = $1
            ORDER BY version_no DESC
            LIMIT 1
        ),
        ins AS (
            INSERT INTO wiki_doc_revisions
                (doc_id, version_no, title, body_md, body_hash, tags,
                 source, author, edit_summary)
            SELECT $1,
                   COALESCE((SELECT version_no FROM last), 0) + 1,
                   $2, $3, $4, $5, $6, $7, $8
            WHERE NOT EXISTS (SELECT 1 FROM last WHERE body_hash = $4)
            RETURNING version_no
        ),
        bump_base AS (
            UPDATE wiki_docs
               SET current_version = (SELECT version_no FROM ins),
                   updated_at = NOW()
             WHERE id = $1 AND EXISTS (SELECT 1 FROM ins)
            RETURNING 1
        )
        SELECT COALESCE(
            (SELECT version_no FROM ins),
            (SELECT version_no FROM last),
            0
        ) AS version_no
    "#;

    let version: i32 = sqlx::query_scalar(sql)
        .bind(doc_id)
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
