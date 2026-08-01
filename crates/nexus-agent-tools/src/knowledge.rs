//! MCP tools per la Knowledge Base per-progetto.
//!
//! ADR 0017 v2 TODO 2 — Reimplementazione 1:1 dei 9 tool `knowledge_*` sul nuovo
//! schema unificato (`wiki_docs` + `wiki_links` + `wiki_concept_triples`).
//!
//! Le firme pubbliche restano stabili: i tool sono esposti via
//! `NexusToolCatalog` e gli agenti AI in produzione si aspettano i campi
//! documentati in `agent_tools::tool_schema` (es. `note_id`, `intent`,
//! `rel_type`, `outgoing`, `incoming`). Solo le implementazioni interne sono
//! state riscritte: le query SQL puntano alle nuove tabelle, gli embedding
//! usano la collection Qdrant `wiki_content`, e gli scope sono sempre
//! `WikiScope::Project` con `project_id = ctx.project_id`.
//!
//! Mapping concettuale vecchio -> nuovo:
//!   - `project_knowledge_notes`       -> `wiki_docs` (scope='project')
//!   - `project_knowledge_links`       -> `wiki_links`
//!   - `note.off_topic = true`         -> `wiki_docs.edit_lock = 'frozen'`
//!   - `note.intent`                   -> `wiki_docs.intent` (campo legacy preservato)
//!   - `note.kind = 'code_doc'`        -> `wiki_docs.kind = 'code_doc'`
//!   - `rel_type` mapping (note->link):
//!       followup     -> followup
//!       correction   -> correction_of
//!       refinement   -> refines
//!       duplicate    -> duplicate_of
//!       blocks       -> blocks
//!       blocked_by   -> blocked_by
//!       relates      -> relates

use crate::context_core::ToolContextCore;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

/// rel_type esposti agli agenti (schema stabile). Lo stesso set della vecchia
/// `knowledge.rel_type`: gli agenti gia' deployati ne dipendono.
pub const KNOWLEDGE_REL_TYPES: [&str; 7] = [
    "followup",
    "correction",
    "refinement",
    "duplicate",
    "blocks",
    "blocked_by",
    "relates",
];

/// Traduce il rel_type "agente-facing" verso il vocabolario di `wiki_links`.
fn map_rel_to_wiki(rel: &str) -> &'static str {
    match rel {
        "followup" => "followup",
        "correction" => "correction_of",
        "refinement" => "refines",
        "duplicate" => "duplicate_of",
        "blocks" => "blocks",
        "blocked_by" => "blocked_by",
        "relates" => "relates",
        _ => "relates",
    }
}

/// Traduzione inversa (storage -> esposizione agente). Sconosciuti restano
/// inalterati (es. `mentions`, `implements`, `tests` — emessi dai worker auto)
/// in modo che l'agente vede anche le relazioni nuove dell'ADR 0017 v2.
fn map_rel_from_wiki(rel: &str) -> String {
    match rel {
        "correction_of" => "correction".to_string(),
        "refines" => "refinement".to_string(),
        "duplicate_of" => "duplicate".to_string(),
        other => other.to_string(),
    }
}

/// Filtro Qdrant standard per i doc del progetto corrente (scope + project_id).
/// Punto unico del payload di filtro riusato da search e subgraph.
fn project_qdrant_filter(project_id: Uuid) -> Value {
    json!({
        "must": [
            { "key": "scope", "match": { "value": "project" } },
            { "key": "project_id", "match": { "value": project_id.to_string() } }
        ]
    })
}

/// Tronca il testo da embeddare al limite (2000 char) senza copiarlo se corto.
fn embed_slice(text: &str) -> &str {
    if text.len() > 2000 {
        &text[..2000]
    } else {
        text
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_search
// ═══════════════════════════════════════════════════════════════════════════

/// Parametri validati di `knowledge_search`.
struct SearchParams {
    query: String,
    top_k: usize,
    min_score: f32,
}

/// Estrae e valida gli input di `knowledge_search`. `Err` = messaggio JSON di
/// errore gia' serializzato (contratto invariato).
fn parse_search_params(input: &Value) -> Result<SearchParams, String> {
    let query = match input.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.trim().is_empty() => q.trim().to_string(),
        _ => return Err(crate::errore_json("query mancante o vuota")),
    };
    let top_k = input
        .get("top_k")
        .and_then(|v| v.as_u64())
        .unwrap_or(5)
        .clamp(1, 100) as usize;
    let min_score = input
        .get("min_score")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.4) as f32;
    Ok(SearchParams {
        query,
        top_k,
        min_score,
    })
}

/// Soglia summary-mode (DB-driven, regola G — niente fallback hardcoded sopra il
/// safe default 20).
async fn search_summary_threshold(ctx: &ToolContextCore) -> usize {
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'agent.kb.graph_summary_threshold_topk'",
    )
    .fetch_optional(&*ctx.db)
    .await
    .ok()
    .flatten()
    .and_then(|v| v.trim().parse().ok())
    .unwrap_or(20)
}

/// Serializza le righe di cluster (theme/count/sample_titles) e ne somma i
/// count. Ritorna `(clusters_json, total)`.
fn cluster_rows_to_json(rows: &[sqlx::postgres::PgRow]) -> (Vec<Value>, i32) {
    let clusters: Vec<Value> = rows
        .iter()
        .map(|r| {
            let theme: Option<String> = r.try_get("theme").ok();
            let count: i32 = r.try_get("count").unwrap_or(0);
            let titles: Vec<String> = r.try_get("sample_titles").unwrap_or_default();
            json!({
                "theme": theme.unwrap_or_else(|| "other".to_string()),
                "count": count,
                "sample_titles": titles,
            })
        })
        .collect();
    let total: i32 = clusters
        .iter()
        .filter_map(|c| c.get("count").and_then(|v| v.as_i64()))
        .sum::<i64>() as i32;
    (clusters, total)
}

/// Summary-mode: cluster per `intent` (o `kind` se intent assente) sui doc del
/// progetto. Esclude i doc 'frozen' (semantica equivalente al vecchio off_topic).
async fn knowledge_search_summary(ctx: &ToolContextCore, top_k: usize) -> String {
    let rows = sqlx::query(
        r#"
        WITH ranked AS (
            SELECT COALESCE(intent, kind) AS theme, title,
                   row_number() OVER (PARTITION BY COALESCE(intent, kind)
                                      ORDER BY updated_at DESC) AS rk
            FROM wiki_docs
            WHERE scope = 'project' AND project_id = $1
              AND edit_lock <> 'frozen'
        )
        SELECT theme,
               COUNT(*)::int AS count,
               array_agg(title ORDER BY rk) FILTER (WHERE rk <= 3) AS sample_titles
        FROM ranked
        GROUP BY theme
        ORDER BY count DESC
        LIMIT $2
        "#,
    )
    .bind(ctx.project_id)
    .bind(top_k as i32)
    .fetch_all(&*ctx.db)
    .await;
    let rows = match rows {
        Ok(r) => r,
        Err(e) => return crate::errore_json(format!("DB cluster query: {e}")),
    };
    let (clusters, total) = cluster_rows_to_json(&rows);
    json!({
        "mode": "summary",
        "clusters": clusters,
        "total": total,
        "hint": "Per body completo di un cluster: knowledge_search(query, top_k<=20)."
    })
    .to_string()
}

/// Ricerca semantica via embedding Qdrant: ritorna gli hit (doc_id, score)
/// sopra soglia gia' filtrati a `top_k`. `Err` = JSON di errore serializzato.
async fn knowledge_search_hits(
    ctx: &ToolContextCore,
    p: &SearchParams,
) -> Result<Vec<(Uuid, f32)>, String> {
    let vector = match ctx.embedder.embed_text("", embed_slice(&p.query)).await {
        Ok(v) => v,
        Err(e) => return Err(crate::errore_json(format!("embed fallito: {e}"))),
    };
    let hits = match nexus_wiki::content_points::search_wiki_content_points_filtered(
        &ctx.db,
        vector,
        (p.top_k * 2).max(10),
        p.min_score as f64,
        Some(project_qdrant_filter(ctx.project_id)),
    )
    .await
    {
        Ok(h) => h,
        Err(e) => return Err(crate::errore_json(format!("search Qdrant fallita: {e}"))),
    };
    Ok(hits
        .iter()
        .filter(|h| (h.score as f32) >= p.min_score)
        .filter_map(|h| {
            h.point_id
                .parse::<Uuid>()
                .ok()
                .map(|id| (id, h.score as f32))
        })
        .take(p.top_k)
        .collect())
}

/// Serializza una riga di risultato ricerca nel formato agente-facing. Lo
/// `status` e' sempre "active" (i frozen sono gia' filtrati a monte); il campo
/// resta per non rompere il contratto del tool.
fn search_row_to_json(id: Uuid, r: &sqlx::postgres::PgRow) -> Value {
    let body: String = r.try_get("body_md").unwrap_or_default();
    let snippet = body.chars().take(300).collect::<String>();
    json!({
        "note_id": id.to_string(),
        "title": r.try_get::<String, _>("title").unwrap_or_default(),
        "intent": r.try_get::<Option<String>, _>("intent").ok().flatten()
            .or_else(|| r.try_get::<String, _>("kind").ok()),
        "status": "active",
        "tags": r.try_get::<Vec<String>, _>("tags").unwrap_or_default(),
        "snippet": snippet,
        "truncated": body.len() > 300,
    })
}

/// Idrata i doc-hit con i metadati da `wiki_docs`, escludendo i frozen, e
/// preserva l'ordine per score. `Err` = JSON di errore serializzato.
async fn knowledge_search_render(
    ctx: &ToolContextCore,
    doc_hits: &[(Uuid, f32)],
) -> Result<Vec<Value>, String> {
    let ids: Vec<Uuid> = doc_hits.iter().map(|(id, _)| *id).collect();
    let rows = match sqlx::query(
        r#"
        SELECT id, title, body_md, tags, intent, kind, edit_lock
        FROM wiki_docs
        WHERE id = ANY($1) AND scope = 'project' AND project_id = $2
          AND edit_lock <> 'frozen'
        "#,
    )
    .bind(&ids)
    .bind(ctx.project_id)
    .fetch_all(&*ctx.db)
    .await
    {
        Ok(r) => r,
        Err(e) => return Err(crate::errore_json(format!("DB query: {e}"))),
    };

    let mut by_id: std::collections::HashMap<Uuid, Value> = std::collections::HashMap::new();
    for r in &rows {
        let id: Uuid = match r.try_get("id") {
            Ok(v) => v,
            Err(_) => continue,
        };
        by_id.insert(id, search_row_to_json(id, r));
    }

    Ok(doc_hits
        .iter()
        .filter_map(|(id, score)| {
            by_id.get(id).map(|note| {
                let mut n = note.clone();
                n["score"] = json!(*score);
                n
            })
        })
        .collect())
}

/// `knowledge_search` — top-K doc rilevanti via embedding Qdrant.
///
/// Input: { query, top_k?=5 (1..=100), min_score?=0.4 }.
/// Output: { results: [{note_id,title,intent,status,tags,score,snippet}], count }
/// oppure (top_k > soglia) { mode:"summary", clusters:[{theme,count,sample_titles}] }.
pub async fn tool_knowledge_search(ctx: &ToolContextCore, input: &Value) -> String {
    let params = match parse_search_params(input) {
        Ok(p) => p,
        Err(e) => return e,
    };

    if params.top_k > search_summary_threshold(ctx).await {
        return knowledge_search_summary(ctx, params.top_k).await;
    }

    let doc_hits = match knowledge_search_hits(ctx, &params).await {
        Ok(h) => h,
        Err(e) => return e,
    };
    if doc_hits.is_empty() {
        return json!({"results": [], "message": "nessun documento trovato sopra la soglia"})
            .to_string();
    }

    let results = match knowledge_search_render(ctx, &doc_hits).await {
        Ok(r) => r,
        Err(e) => return e,
    };
    json!({"results": results, "count": results.len()}).to_string()
}

// ═══════════════════════════════════════════════════════════════════════════
// code_doc
// ═══════════════════════════════════════════════════════════════════════════

/// `code_doc` — documentazione code-wiki di un file. Cerca doc con
/// `kind='code_doc'` il cui `vault_file_path` o `title` matcha `file_path`.
pub async fn tool_code_doc(ctx: &ToolContextCore, input: &Value) -> String {
    let file_path = match input.get("file_path").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.trim(),
        _ => return crate::errore_json("file_path mancante o vuoto"),
    };

    let row = sqlx::query(
        r#"
        SELECT id, title, body_md FROM wiki_docs
        WHERE scope = 'project' AND project_id = $1 AND kind = 'code_doc'
          AND (title = $2 OR title LIKE $3 OR $2 LIKE '%' || title
               OR vault_file_path = $2 OR vault_file_path LIKE $3)
        ORDER BY (title = $2 OR vault_file_path = $2) DESC, updated_at DESC
        LIMIT 1
        "#,
    )
    .bind(ctx.project_id)
    .bind(file_path)
    .bind(format!("%{file_path}"))
    .fetch_optional(&*ctx.db)
    .await;

    match row {
        Ok(Some(r)) => {
            let title: String = r.try_get("title").unwrap_or_default();
            let body: String = r.try_get("body_md").unwrap_or_default();
            json!({ "file": title, "found": true, "body": body }).to_string()
        }
        Ok(None) => json!({
            "file": file_path,
            "found": false,
            "message": "Nessuna documentazione (code_doc) per questo file. Prova knowledge_search per contesto correlato."
        })
        .to_string(),
        Err(e) => crate::errore_json(format!("query fallita: {e}")),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_get_note
// ═══════════════════════════════════════════════════════════════════════════

/// Costruisce il JSON di risposta di `knowledge_get_note` a partire dalla riga
/// `wiki_docs`. Isola il mapping (status, file_paths da tag "file:").
fn knowledge_note_json(note_id: Uuid, row: &sqlx::postgres::PgRow) -> Value {
    let edit_lock: String = row
        .try_get("edit_lock")
        .unwrap_or_else(|_| "none".to_string());
    let status = if edit_lock == "frozen" {
        "off_topic"
    } else {
        "active"
    };
    json!({
        "id": note_id.to_string(),
        "title": row.try_get::<String, _>("title").unwrap_or_default(),
        "body_md": row.try_get::<String, _>("body_md").unwrap_or_default(),
        "intent": row.try_get::<Option<String>, _>("intent").ok().flatten()
            .or_else(|| row.try_get::<String, _>("kind").ok()),
        "status": status,
        "tags": row.try_get::<Vec<String>, _>("tags").unwrap_or_default(),
        // file_paths: ricostruiti da tag con prefisso "file:" (se presenti),
        // best-effort per compatibilita' col contratto vecchio.
        "file_paths": row
            .try_get::<Vec<String>, _>("tags")
            .unwrap_or_default()
            .into_iter()
            .filter_map(|t| t.strip_prefix("file:").map(String::from))
            .collect::<Vec<_>>(),
    })
}

/// `knowledge_get_note` — body completo di un doc by id (scoped al progetto).
pub async fn tool_knowledge_get_note(ctx: &ToolContextCore, input: &Value) -> String {
    let note_id = match input
        .get("note_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(id) => id,
        None => return crate::errore_json("note_id mancante o non UUID valido"),
    };

    let row = match sqlx::query(
        r#"
        SELECT id, title, body_md, intent, kind, tags, edit_lock,
               created_at, updated_at
        FROM wiki_docs
        WHERE id = $1 AND scope = 'project' AND project_id = $2
        "#,
    )
    .bind(note_id)
    .bind(ctx.project_id)
    .fetch_optional(&*ctx.db)
    .await
    {
        Ok(Some(r)) => r,
        Ok(None) => return crate::errore_json("nota non trovata o non accessibile"),
        Err(e) => return crate::errore_json(format!("DB: {e}")),
    };

    knowledge_note_json(note_id, &row).to_string()
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_create_note
// ═══════════════════════════════════════════════════════════════════════════

/// Input validati di `knowledge_create_note`.
struct CreateNoteParams {
    title: String,
    body_md: String,
    intent: String,
    tags: Vec<String>,
}

/// Valida e normalizza gli input di `knowledge_create_note`. I `file_paths`
/// diventano tag con prefisso "file:" (preserva l'info nel nuovo schema).
/// `Err` = messaggio JSON di errore serializzato.
fn parse_create_note_params(input: &Value) -> Result<CreateNoteParams, String> {
    let title = match input
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 200)
    {
        Some(t) => t.to_string(),
        None => return Err(crate::errore_json("title mancante o invalido (1-200 char)")),
    };
    let body_md = match input
        .get("body_md")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(b) => b.to_string(),
        None => return Err(crate::errore_json("body_md mancante")),
    };
    let intent = input
        .get("intent")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("feature")
        .to_string();
    let mut tags: Vec<String> = input
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if let Some(arr) = input.get("file_paths").and_then(|v| v.as_array()) {
        for v in arr {
            if let Some(s) = v.as_str() {
                if !s.is_empty() {
                    tags.push(format!("file:{s}"));
                }
            }
        }
    }
    Ok(CreateNoteParams {
        title,
        body_md,
        intent,
        tags,
    })
}

/// Upsert del doc in `wiki_docs` (scope=project, kind='note'). Ritorna l'id del
/// doc creato/aggiornato. `Err` = messaggio JSON di errore serializzato.
async fn insert_note_doc(
    ctx: &ToolContextCore,
    p: &CreateNoteParams,
    slug: &str,
    body_hash: &str,
) -> Result<Uuid, String> {
    // kind = 'note' fisso; intent porta la categoria semantica.
    let doc_row: Result<Uuid, _> = sqlx::query_scalar(
        r#"
        INSERT INTO wiki_docs (
            scope, project_id, slug, title, body_md, body_hash,
            kind, intent, tags,
            edit_lock, protected_sections, manually_edited,
            current_version, auto_generated, public_read
        ) VALUES (
            'project', $1, $2, $3, $4, $5,
            $6, $7, $8,
            'none', '{}', FALSE,
            1, FALSE, FALSE
        )
        ON CONFLICT (scope, COALESCE(project_id::text,''), slug) DO UPDATE SET
            title    = EXCLUDED.title,
            body_md  = EXCLUDED.body_md,
            body_hash= EXCLUDED.body_hash,
            tags     = EXCLUDED.tags,
            intent   = EXCLUDED.intent,
            updated_at = NOW()
        RETURNING id
        "#,
    )
    .bind(ctx.project_id)
    .bind(slug)
    .bind(&p.title)
    .bind(&p.body_md)
    .bind(body_hash)
    .bind("note")
    .bind(&p.intent)
    .bind(&p.tags)
    .fetch_one(&*ctx.db)
    .await;
    doc_row.map_err(|e| crate::errore_json(format!("scrittura wiki_docs fallita: {e}")))
}

/// Embedding + upsert Qdrant del doc (best-effort). Ritorna `true` se il punto
/// e' stato indicizzato. Non propaga errori: logga WARN e ritorna `false`.
async fn index_note_qdrant(ctx: &ToolContextCore, note_id: Uuid, p: &CreateNoteParams) -> bool {
    let snippet = embed_slice(&p.body_md);
    let combined = format!("{}\n\n{snippet}", p.title);
    let vector = match ctx.embedder.embed_text("", &combined).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "knowledge_create_note: embed fallito");
            return false;
        }
    };
    let point_id = note_id.to_string();
    let payload = json!({
        "scope": "project",
        "doc_id": point_id,
        "project_id": ctx.project_id.to_string(),
        "title": p.title,
        "tags": p.tags,
        "kind": "note",
        "intent": p.intent,
    });
    match nexus_wiki::content_points::upsert_wiki_content_point(&ctx.db, &point_id, vector, payload).await
    {
        Ok(_) => {
            let _ = sqlx::query("UPDATE wiki_docs SET qdrant_point_id = $1 WHERE id = $2")
                .bind(&point_id)
                .bind(note_id)
                .execute(&*ctx.db)
                .await;
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "knowledge_create_note: Qdrant upsert fallito");
            false
        }
    }
}

/// `knowledge_create_note` — crea un doc scope=project + embedding Qdrant.
pub async fn tool_knowledge_create_note(ctx: &ToolContextCore, input: &Value) -> String {
    let params = match parse_create_note_params(input) {
        Ok(p) => p,
        Err(e) => return e,
    };

    // Slug derivato dal title (slugify minimal: lowercase + replace).
    let slug = nexus_wiki::vault::slugify(&params.title);
    if slug.is_empty() {
        return crate::errore_json("title non genera slug valido");
    }
    let body_hash = nexus_wiki::vault::sha256_hex(&params.body_md);

    let note_id = match insert_note_doc(ctx, &params, &slug, &body_hash).await {
        Ok(id) => id,
        Err(e) => return e,
    };

    let qdrant_indexed = index_note_qdrant(ctx, note_id, &params).await;

    tracing::info!(
        project_id = %ctx.project_id,
        note_id = %note_id,
        intent = %params.intent,
        "knowledge_create_note: doc creato via MCP tool (wiki_docs)"
    );

    json!({
        "ok": true,
        "note_id": note_id.to_string(),
        "intent": params.intent,
        "qdrant_indexed": qdrant_indexed,
    })
    .to_string()
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_get_links
// ═══════════════════════════════════════════════════════════════════════════

/// Verifica che il doc appartenga al progetto corrente.
async fn note_in_project(ctx: &ToolContextCore, note_id: Uuid) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM wiki_docs \
         WHERE id = $1 AND scope = 'project' AND project_id = $2",
    )
    .bind(note_id)
    .bind(ctx.project_id)
    .fetch_one(&*ctx.db)
    .await
    .unwrap_or(0)
        > 0
}

/// Carica i link in una direzione (`outgoing`=true: edge da `note_id`;
/// altrimenti verso `note_id`) verso doc visibili al progetto (proprio progetto
/// o meta public_read=true), escludendo i doc frozen.
async fn load_directional_links(
    ctx: &ToolContextCore,
    note_id: Uuid,
    outgoing: bool,
) -> Vec<sqlx::postgres::PgRow> {
    // Le due direzioni differiscono solo per la colonna di ancoraggio e il JOIN.
    let sql = if outgoing {
        r#"
        SELECT l.from_doc_id, l.to_doc_id AS other_id, l.rel_type, l.created_by,
               l.confidence, d.title, d.intent, d.kind, d.scope, d.edit_lock
        FROM wiki_links l
        JOIN wiki_docs d ON d.id = l.to_doc_id
        WHERE l.from_doc_id = $1
          AND ( (d.scope = 'project' AND d.project_id = $2)
                OR (d.scope = 'meta' AND d.public_read = TRUE) )
          AND d.edit_lock <> 'frozen'
        ORDER BY l.confidence DESC
        "#
    } else {
        r#"
        SELECT l.to_doc_id, l.from_doc_id AS other_id, l.rel_type, l.created_by,
               l.confidence, d.title, d.intent, d.kind, d.scope, d.edit_lock
        FROM wiki_links l
        JOIN wiki_docs d ON d.id = l.from_doc_id
        WHERE l.to_doc_id = $1
          AND ( (d.scope = 'project' AND d.project_id = $2)
                OR (d.scope = 'meta' AND d.public_read = TRUE) )
          AND d.edit_lock <> 'frozen'
        ORDER BY l.confidence DESC
        "#
    };
    sqlx::query(sql)
        .bind(note_id)
        .bind(ctx.project_id)
        .fetch_all(&*ctx.db)
        .await
        .unwrap_or_default()
}

/// Serializza le righe di link nel formato agente-facing (rel_type tradotto).
fn links_to_json(rows: &[sqlx::postgres::PgRow]) -> Vec<Value> {
    rows.iter()
        .filter_map(|r| {
            let other = r.try_get::<Uuid, _>("other_id").ok()?;
            let stored_rel: String = r.try_get("rel_type").unwrap_or_default();
            Some(json!({
                "note_id": other.to_string(),
                "title": r.try_get::<String, _>("title").unwrap_or_default(),
                "intent": r.try_get::<Option<String>, _>("intent").ok().flatten()
                    .or_else(|| r.try_get::<String, _>("kind").ok()),
                "rel_type": map_rel_from_wiki(&stored_rel),
                "rel_type_raw": stored_rel,
                "created_by": r.try_get::<String, _>("created_by").unwrap_or_default(),
                "confidence": r.try_get::<f32, _>("confidence").unwrap_or(1.0),
                "scope": r.try_get::<String, _>("scope").unwrap_or_default(),
            }))
        })
        .collect()
}

/// `knowledge_get_links` — outbound + inbound links di un doc, scoped al progetto.
pub async fn tool_knowledge_get_links(ctx: &ToolContextCore, input: &Value) -> String {
    let note_id = match input
        .get("note_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(id) => id,
        None => return crate::errore_json("note_id mancante o non UUID valido"),
    };

    if !note_in_project(ctx, note_id).await {
        return crate::errore_json("nota non trovata nel progetto corrente");
    }

    let out = links_to_json(&load_directional_links(ctx, note_id, true).await);
    let inc = links_to_json(&load_directional_links(ctx, note_id, false).await);
    json!({
        "note_id": note_id.to_string(),
        "outgoing": out,
        "incoming": inc,
        "outgoing_count": out.len(),
        "incoming_count": inc.len(),
    })
    .to_string()
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_get_subgraph
// ═══════════════════════════════════════════════════════════════════════════

/// Parametri validati di `knowledge_get_subgraph`.
struct SubgraphParams {
    max_nodes: usize,
    depth: usize,
    /// rel_type gia' mappati al vocabolario `wiki_links` per la query.
    rel_filter_wiki: Vec<String>,
}

/// Estrae e valida i parametri comuni di `knowledge_get_subgraph`.
fn parse_subgraph_params(input: &Value) -> SubgraphParams {
    let max_nodes = input
        .get("max_nodes")
        .and_then(|v| v.as_u64())
        .unwrap_or(30)
        .clamp(1, 100) as usize;
    let depth = input
        .get("depth")
        .and_then(|v| v.as_u64())
        .unwrap_or(2)
        .clamp(1, 4) as usize;
    let rel_filter_input: Vec<String> = input
        .get("rel_types")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .filter(|s| KNOWLEDGE_REL_TYPES.contains(s))
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| KNOWLEDGE_REL_TYPES.iter().map(|s| s.to_string()).collect());
    let rel_filter_wiki = rel_filter_input
        .iter()
        .map(|r| map_rel_to_wiki(r).to_string())
        .collect();
    SubgraphParams {
        max_nodes,
        depth,
        rel_filter_wiki,
    }
}

/// Risolve i nodi seed: da `query` (semantica via Qdrant) o da `note_id`.
/// `Err` = messaggio JSON di errore serializzato (mancanza seed o embed fallito).
async fn resolve_subgraph_seed(
    ctx: &ToolContextCore,
    input: &Value,
    max_nodes: usize,
) -> Result<Vec<Uuid>, String> {
    let mut nodes: Vec<Uuid> = Vec::new();
    if let Some(q) = input
        .get("query")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let vector = match ctx.embedder.embed_text("", embed_slice(q)).await {
            Ok(v) => v,
            Err(e) => return Err(crate::errore_json(format!("embed fallito: {e}"))),
        };
        let hits = nexus_wiki::content_points::search_wiki_content_points_filtered(
            &ctx.db,
            vector,
            max_nodes,
            0.0,
            Some(project_qdrant_filter(ctx.project_id)),
        )
        .await
        .unwrap_or_default();
        for h in hits.iter() {
            if let Ok(id) = h.point_id.parse::<Uuid>() {
                if !nodes.contains(&id) {
                    nodes.push(id);
                }
            }
        }
    } else if let Some(id) = input
        .get("note_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        nodes.push(id);
    } else {
        return Err(
            crate::errore_json("serve 'query' (testo) oppure 'note_id' (UUID) come seed"),
        );
    }
    Ok(nodes)
}

/// BFS via `wiki_links` a partire dai nodi seed, fino a `depth` livelli o
/// `max_nodes` nodi. Muta `nodes` aggiungendo i vicini scoperti.
async fn expand_subgraph_bfs(ctx: &ToolContextCore, p: &SubgraphParams, nodes: &mut Vec<Uuid>) {
    let mut frontier = nodes.clone();
    for _ in 0..p.depth {
        if nodes.len() >= p.max_nodes {
            break;
        }
        let neigh = sqlx::query(
            r#"
            SELECT from_doc_id, to_doc_id FROM wiki_links
            WHERE rel_type = ANY($1)
              AND (from_doc_id = ANY($2) OR to_doc_id = ANY($2))
            "#,
        )
        .bind(&p.rel_filter_wiki)
        .bind(&*frontier)
        .fetch_all(&*ctx.db)
        .await
        .unwrap_or_default();
        let mut next: Vec<Uuid> = Vec::new();
        for r in &neigh {
            for col in ["from_doc_id", "to_doc_id"] {
                if let Ok(id) = r.try_get::<Uuid, _>(col) {
                    if !nodes.contains(&id) && !next.contains(&id) {
                        next.push(id);
                    }
                }
            }
        }
        if next.is_empty() {
            break;
        }
        for id in next.iter() {
            if nodes.len() < p.max_nodes {
                nodes.push(*id);
            }
        }
        frontier = next;
    }
}

/// Dettagli dei nodi validi (scope=project + project_id + non-frozen). Ritorna
/// gli id validi e la loro serializzazione JSON.
async fn subgraph_nodes(ctx: &ToolContextCore, nodes: &[Uuid]) -> (Vec<Uuid>, Vec<Value>) {
    let rows = sqlx::query(
        r#"
        SELECT id, title, intent, kind, edit_lock FROM wiki_docs
        WHERE id = ANY($1) AND scope = 'project' AND project_id = $2
          AND edit_lock <> 'frozen'
        "#,
    )
    .bind(nodes)
    .bind(ctx.project_id)
    .fetch_all(&*ctx.db)
    .await
    .unwrap_or_default();
    let valid_ids: Vec<Uuid> = rows
        .iter()
        .filter_map(|r| r.try_get::<Uuid, _>("id").ok())
        .collect();
    let node_json: Vec<Value> = rows
        .iter()
        .filter_map(|r| {
            let id = r.try_get::<Uuid, _>("id").ok()?;
            Some(json!({
                "note_id": id.to_string(),
                "title": r.try_get::<String, _>("title").unwrap_or_default(),
                "intent": r.try_get::<Option<String>, _>("intent").ok().flatten()
                    .or_else(|| r.try_get::<String, _>("kind").ok()),
                "status": "active",
            }))
        })
        .collect();
    (valid_ids, node_json)
}

/// Archi intra-sottografo tra i nodi validi, serializzati per l'agente.
async fn subgraph_edges(
    ctx: &ToolContextCore,
    rel_filter_wiki: &[String],
    valid_ids: &[Uuid],
) -> Vec<Value> {
    let edges = sqlx::query(
        r#"
        SELECT from_doc_id, to_doc_id, rel_type, confidence FROM wiki_links
        WHERE rel_type = ANY($1)
          AND from_doc_id = ANY($2) AND to_doc_id = ANY($2)
        "#,
    )
    .bind(rel_filter_wiki)
    .bind(valid_ids)
    .fetch_all(&*ctx.db)
    .await
    .unwrap_or_default();
    edges
        .iter()
        .filter_map(|r| {
            let f = r.try_get::<Uuid, _>("from_doc_id").ok()?;
            let t = r.try_get::<Uuid, _>("to_doc_id").ok()?;
            let stored_rel: String = r.try_get("rel_type").unwrap_or_default();
            Some(json!({
                "from": f.to_string(),
                "to": t.to_string(),
                "rel_type": map_rel_from_wiki(&stored_rel),
                "rel_type_raw": stored_rel,
                "confidence": r.try_get::<f32, _>("confidence").unwrap_or(1.0),
            }))
        })
        .collect()
}

/// `knowledge_get_subgraph` — BFS dal seed (query semantica o note_id) sui link.
pub async fn tool_knowledge_get_subgraph(ctx: &ToolContextCore, input: &Value) -> String {
    let params = parse_subgraph_params(input);

    let mut nodes = match resolve_subgraph_seed(ctx, input, params.max_nodes).await {
        Ok(n) => n,
        Err(e) => return e,
    };
    if nodes.is_empty() {
        return json!({"nodes": [], "edges": [], "message": "nessun nodo seed trovato"})
            .to_string();
    }

    expand_subgraph_bfs(ctx, &params, &mut nodes).await;

    let (valid_ids, node_json) = subgraph_nodes(ctx, &nodes).await;
    let edge_json = subgraph_edges(ctx, &params.rel_filter_wiki, &valid_ids).await;

    json!({
        "nodes": node_json,
        "edges": edge_json,
        "node_count": node_json.len(),
        "edge_count": edge_json.len(),
    })
    .to_string()
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_create_link
// ═══════════════════════════════════════════════════════════════════════════

/// Input validati di `knowledge_create_link`.
struct CreateLinkParams {
    from: Uuid,
    to: Uuid,
    /// rel_type agente-facing (per l'output).
    rel_input: String,
    /// rel_type mappato al vocabolario `wiki_links` (per lo storage).
    rel_wiki: &'static str,
    confidence: f32,
}

/// Valida e normalizza gli input di `knowledge_create_link`.
/// `Err` = messaggio JSON di errore serializzato.
fn parse_create_link_params(input: &Value) -> Result<CreateLinkParams, String> {
    let from = match input
        .get("from_note_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(id) => id,
        None => return Err(crate::errore_json("from_note_id mancante o non UUID valido")),
    };
    let to = match input
        .get("to_note_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(id) => id,
        None => return Err(crate::errore_json("to_note_id mancante o non UUID valido")),
    };
    if from == to {
        return Err(crate::errore_json("self-link non ammesso (from == to)"));
    }
    let rel_input = match input.get("rel_type").and_then(|v| v.as_str()) {
        Some(r) if KNOWLEDGE_REL_TYPES.contains(&r) => r.to_string(),
        Some(r) => {
            return Err(crate::errore_json(format!(
                "rel_type '{r}' non valido; ammessi: {KNOWLEDGE_REL_TYPES:?}"
            )))
        }
        None => return Err(crate::errore_json("rel_type mancante")),
    };
    let rel_wiki = map_rel_to_wiki(&rel_input);
    let confidence = input
        .get("confidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0)
        .clamp(0.0, 1.0) as f32;
    Ok(CreateLinkParams {
        from,
        to,
        rel_input,
        rel_wiki,
        confidence,
    })
}

/// Verifica che entrambi i doc esistano e siano accessibili dal progetto
/// (entrambi project corrente, oppure to_note appartiene a meta public).
async fn both_docs_accessible(ctx: &ToolContextCore, from: Uuid, to: Uuid) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM wiki_docs \
         WHERE id = ANY($1) \
           AND ( (scope='project' AND project_id = $2) \
                 OR (scope='meta' AND public_read = TRUE) )",
    )
    .bind(vec![from, to])
    .bind(ctx.project_id)
    .fetch_one(&*ctx.db)
    .await
    .unwrap_or(0)
        == 2
}

/// `knowledge_create_link` — crea o aggiorna un link tra due doc del progetto.
pub async fn tool_knowledge_create_link(ctx: &ToolContextCore, input: &Value) -> String {
    let p = match parse_create_link_params(input) {
        Ok(p) => p,
        Err(e) => return e,
    };

    if !both_docs_accessible(ctx, p.from, p.to).await {
        return crate::errore_json("una o entrambe le note non esistono nel progetto corrente");
    }

    // wiki_links: PK = (from_doc_id, to_doc_id, rel_type). ON CONFLICT update
    // della confidence/created_by.
    let res = sqlx::query(
        r#"
        INSERT INTO wiki_links (from_doc_id, to_doc_id, rel_type, created_by, confidence, evidence)
        VALUES ($1, $2, $3, 'agent', $4, 'agent tool knowledge_create_link')
        ON CONFLICT (from_doc_id, to_doc_id, rel_type)
        DO UPDATE SET confidence = EXCLUDED.confidence, created_by = 'agent'
        "#,
    )
    .bind(p.from)
    .bind(p.to)
    .bind(p.rel_wiki)
    .bind(p.confidence)
    .execute(&*ctx.db)
    .await;

    match res {
        Ok(_) => json!({
            "ok": true,
            "from_note_id": p.from.to_string(),
            "to_note_id": p.to.to_string(),
            "rel_type": p.rel_input,
            "rel_type_raw": p.rel_wiki,
        })
        .to_string(),
        // "INSERT" qui e' il prefisso di un messaggio d'errore diagnostico, non
        // una query costruita per interpolazione: la INSERT sopra e' interamente
        // parametrizzata via .bind(). Il messaggio evita la keyword SQL letterale.
        Err(e) => crate::errore_json(format!("creazione link fallita: {e}")),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_set_relevance
// ═══════════════════════════════════════════════════════════════════════════

/// `knowledge_set_relevance` — marca un doc come off-topic (`edit_lock='frozen'`)
/// o on-topic (`edit_lock='none'`). Il campo `relevance_score` non e' piu'
/// persistito nel nuovo schema; viene accettato per compatibilita' ma ignorato.
pub async fn tool_knowledge_set_relevance(ctx: &ToolContextCore, input: &Value) -> String {
    let note_id = match input
        .get("note_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(id) => id,
        None => return crate::errore_json("note_id mancante o non UUID valido"),
    };
    let off_topic = match input.get("off_topic").and_then(|v| v.as_bool()) {
        Some(b) => b,
        None => return crate::errore_json("off_topic (bool) mancante"),
    };
    let new_lock = if off_topic { "frozen" } else { "none" };

    let res = sqlx::query(
        r#"
        UPDATE wiki_docs
        SET edit_lock = $2, updated_at = NOW()
        WHERE id = $1 AND scope = 'project' AND project_id = $3
        "#,
    )
    .bind(note_id)
    .bind(new_lock)
    .bind(ctx.project_id)
    .execute(&*ctx.db)
    .await;

    match res {
        Ok(r) if r.rows_affected() > 0 => json!({
            "ok": true,
            "note_id": note_id.to_string(),
            "off_topic": off_topic,
        })
        .to_string(),
        Ok(_) => crate::errore_json("nota non trovata nel progetto corrente"),
        // "UPDATE" qui e' il prefisso di un messaggio d'errore diagnostico, non
        // una query costruita per interpolazione: la UPDATE sopra e' interamente
        // parametrizzata via .bind(). Il messaggio evita la keyword SQL letterale.
        Err(e) => crate::errore_json(format!("aggiornamento rilevanza fallito: {e}")),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_import_graph
// ═══════════════════════════════════════════════════════════════════════════

/// Config di `knowledge_import_graph` letta dai settings (riusa le chiavi
/// storiche; safe defaults se mancanti).
struct GraphImportConfig {
    enabled: bool,
    max_nodes: usize,
}

/// Legge la config di import grafi dai settings.
async fn load_graph_import_config(ctx: &ToolContextCore) -> GraphImportConfig {
    let mut enabled = true;
    let mut max_nodes = 2000usize;
    let rows = sqlx::query(
        "SELECT key, value FROM settings WHERE key IN \
         ('knowledge.graph_import_enabled','knowledge.graph_import_max_nodes')",
    )
    .fetch_all(&*ctx.db)
    .await
    .unwrap_or_default();
    for r in &rows {
        let k: String = r.try_get("key").unwrap_or_default();
        let v: String = r.try_get("value").unwrap_or_default();
        match k.as_str() {
            "knowledge.graph_import_enabled" => {
                enabled = !matches!(v.trim().to_lowercase().as_str(), "false" | "0" | "off");
            }
            "knowledge.graph_import_max_nodes" => {
                max_nodes = v.trim().parse().unwrap_or(2000);
            }
            _ => {}
        }
    }
    GraphImportConfig { enabled, max_nodes }
}

/// Campi normalizzati di un nodo di import pronti per l'INSERT in `wiki_docs`.
struct GraphNode {
    title: String,
    body: String,
    tags: Vec<String>,
    slug: String,
    body_hash: String,
}

/// Estrae e normalizza i campi di un nodo del grafo esterno. `None` se il nodo
/// e' privo di `id` (va saltato).
fn prepare_graph_node(n: &Value, source_id: &str) -> Option<GraphNode> {
    let ext_id = n
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if ext_id.is_empty() {
        return None;
    }
    let label = n
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or(&ext_id)
        .to_string();
    let title: String = label.chars().take(200).collect();
    let body = n
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or(&title)
        .to_string();
    let node_type = n.get("node_type").and_then(|v| v.as_str()).unwrap_or("");
    let mut tags: Vec<String> = Vec::new();
    if !node_type.is_empty() {
        tags.push(node_type.to_string());
    }
    tags.push(format!("ext:{source_id}"));

    // Slug stabile: includes ext_id per evitare collisioni.
    let slug = nexus_wiki::vault::slugify(&format!("imp-{source_id}-{ext_id}"));
    let body_hash = nexus_wiki::vault::sha256_hex(&body);
    Some(GraphNode {
        title,
        body,
        tags,
        slug,
        body_hash,
    })
}

/// Importa un singolo nodo del grafo esterno in `wiki_docs`. Ritorna `Some(id)`
/// del doc creato/aggiornato, `None` se il nodo va saltato o l'insert fallisce.
async fn import_graph_node(ctx: &ToolContextCore, n: &Value, source_id: &str) -> Option<Uuid> {
    let node = prepare_graph_node(n, source_id)?;
    let res: Result<Uuid, _> = sqlx::query_scalar(
        r#"
        INSERT INTO wiki_docs (
            scope, project_id, slug, title, body_md, body_hash,
            kind, intent, tags,
            edit_lock, protected_sections, manually_edited,
            current_version, auto_generated, public_read
        ) VALUES (
            'project', $1, $2, $3, $4, $5,
            'note', 'domain', $6,
            'none', '{}', FALSE,
            1, TRUE, FALSE
        )
        ON CONFLICT (scope, COALESCE(project_id::text,''), slug) DO UPDATE SET
            title=EXCLUDED.title, body_md=EXCLUDED.body_md, body_hash=EXCLUDED.body_hash,
            tags=EXCLUDED.tags, updated_at=NOW()
        RETURNING id
        "#,
    )
    .bind(ctx.project_id)
    .bind(&node.slug)
    .bind(&node.title)
    .bind(&node.body)
    .bind(&node.body_hash)
    .bind(&node.tags)
    .fetch_one(&*ctx.db)
    .await;
    res.ok()
}

/// Traduce l'`edge_type` esterno verso il `rel_type` di `wiki_links`
/// (heuristica semplice).
fn map_edge_type_to_rel(etype: &str) -> &'static str {
    match etype {
        "depends_on" | "requires" | "needs" => "depends_on",
        "blocks" => "blocks",
        "blocked_by" => "blocked_by",
        "implements" => "implements",
        "tests" => "tests",
        "refines" | "refinement" => "refines",
        _ => "relates",
    }
}

/// Importa un singolo arco del grafo esterno in `wiki_links`, risolvendo gli
/// endpoint tramite `id_map`. Ritorna `true` se l'arco e' stato inserito.
async fn import_graph_edge(
    ctx: &ToolContextCore,
    e: &Value,
    id_map: &std::collections::HashMap<String, Uuid>,
) -> bool {
    let source = e.get("source").and_then(|v| v.as_str()).unwrap_or("");
    let target = e.get("target").and_then(|v| v.as_str()).unwrap_or("");
    if source.is_empty() || target.is_empty() {
        return false;
    }
    let (Some(&f), Some(&t)) = (id_map.get(source), id_map.get(target)) else {
        return false;
    };
    if f == t {
        return false;
    }
    let etype = e
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    let rel = map_edge_type_to_rel(&etype);
    sqlx::query(
        r#"
        INSERT INTO wiki_links (from_doc_id, to_doc_id, rel_type, created_by, confidence, evidence)
        VALUES ($1, $2, $3, 'external', 1.0, 'imported via knowledge_import_graph')
        ON CONFLICT (from_doc_id, to_doc_id, rel_type) DO NOTHING
        "#,
    )
    .bind(f)
    .bind(t)
    .bind(rel)
    .execute(&*ctx.db)
    .await
    .is_ok()
}

/// `knowledge_import_graph` — import grafo esterno (JSON/Mermaid/DOT) nella KB.
/// Nodi -> `wiki_docs` (scope=project), archi -> `wiki_links`.
/// Il parser di grafi `knowledge::graph_import` e' stato rimosso assieme al
/// modulo `knowledge/`; per ora supportiamo solo il formato JSON node-link
/// (`{"nodes":[{id,label,content?,node_type?}], "edges":[{source,target,type?}]}`).
/// Input validati di `knowledge_import_graph`.
struct ImportGraphInput {
    format: String,
    content: String,
    source_id: String,
}

/// Estrae e valida gli input base di `knowledge_import_graph` (format, content,
/// source_id). `Err` = messaggio JSON di errore serializzato.
fn parse_import_graph_input(input: &Value) -> Result<ImportGraphInput, String> {
    let format = match input.get("format").and_then(|v| v.as_str()) {
        Some(f) if !f.trim().is_empty() => f.trim().to_lowercase(),
        _ => {
            return Err(
                crate::errore_json("parametro 'format' obbligatorio (json | mermaid | dot)"),
            )
        }
    };
    let content = match input.get("content").and_then(|v| v.as_str()) {
        Some(c) if !c.trim().is_empty() => c.to_string(),
        _ => return Err(crate::errore_json("parametro 'content' obbligatorio")),
    };
    let source_id = input
        .get("source_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("import")
        .to_string();
    Ok(ImportGraphInput {
        format,
        content,
        source_id,
    })
}

/// Esegue i due passi di import (nodi -> `wiki_docs`, archi -> `wiki_links`) e
/// ritorna i conteggi `(nodes_created, edges_created)`.
async fn run_graph_import(
    ctx: &ToolContextCore,
    nodes_in: &[Value],
    edges_in: &[Value],
    source_id: &str,
) -> (usize, usize) {
    let mut id_map: std::collections::HashMap<String, Uuid> = std::collections::HashMap::new();
    let mut nodes_created = 0usize;
    for n in nodes_in {
        if let Some(id) = import_graph_node(ctx, n, source_id).await {
            let ext_id = n.get("id").and_then(|v| v.as_str()).unwrap_or("").trim();
            id_map.insert(ext_id.to_string(), id);
            nodes_created += 1;
        }
    }
    let mut edges_created = 0usize;
    for e in edges_in {
        if import_graph_edge(ctx, e, &id_map).await {
            edges_created += 1;
        }
    }
    (nodes_created, edges_created)
}

/// Fa il parsing del payload JSON node-link e ne valida i nodi (non vuoti,
/// entro `max_nodes`). Ritorna `(nodes_in, edges_in)`. `Err` = messaggio JSON di
/// errore serializzato.
fn parse_graph_payload(
    content: &str,
    max_nodes: usize,
) -> Result<(Vec<Value>, Vec<Value>), String> {
    let payload: Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(e) => return Err(crate::errore_json(format!("JSON invalido: {e}"))),
    };
    let nodes_in = payload
        .get("nodes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let edges_in = payload
        .get("edges")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if nodes_in.is_empty() {
        return Err(crate::errore_json("nessun nodo trovato nel grafo"));
    }
    if nodes_in.len() > max_nodes {
        return Err(
            crate::errore_json(format!("troppi nodi: {} > max {}", nodes_in.len(), max_nodes)),
        );
    }
    Ok((nodes_in, edges_in))
}

pub async fn tool_knowledge_import_graph(ctx: &ToolContextCore, input: &Value) -> String {
    let args = match parse_import_graph_input(input) {
        Ok(a) => a,
        Err(e) => return e,
    };

    let cfg = load_graph_import_config(ctx).await;
    if !cfg.enabled {
        return crate::errore_json(
            "import grafi disabilitato (knowledge.graph_import_enabled=false)",
        );
    }
    if args.format != "json" {
        return crate::errore_json(format!(
                "formato '{}' non supportato in questa versione (solo 'json' node-link). \
                 Mermaid/DOT richiedono il parser legacy `knowledge::graph_import` non ancora portato.",
                args.format
            ));
    }

    // Parsing JSON node-link minimo.
    let (nodes_in, edges_in) = match parse_graph_payload(&args.content, cfg.max_nodes) {
        Ok(v) => v,
        Err(e) => return e,
    };

    let (nodes_created, edges_created) =
        run_graph_import(ctx, &nodes_in, &edges_in, &args.source_id).await;

    tracing::info!(
        project_id = %ctx.project_id,
        format = %args.format,
        nodes_created,
        edges_created,
        "knowledge_import_graph: grafo esterno importato in wiki_docs/wiki_links"
    );

    json!({
        "ok": true,
        "format": args.format,
        "nodes_created": nodes_created,
        "edges_created": edges_created,
        "source_id": args.source_id,
    })
    .to_string()
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_mapping_roundtrip_known() {
        for r in KNOWLEDGE_REL_TYPES.iter() {
            let to_wiki = map_rel_to_wiki(r);
            let back = map_rel_from_wiki(to_wiki);
            assert_eq!(back, *r, "roundtrip rotto su {r}");
        }
    }

    #[test]
    fn rel_mapping_passes_through_unknown_wiki_rels() {
        // I rel emessi dai worker auto (mentions, implements, tests) non hanno
        // un equivalente "agent-facing" e devono passare tal quale al client.
        assert_eq!(map_rel_from_wiki("mentions"), "mentions");
        assert_eq!(map_rel_from_wiki("implements"), "implements");
        assert_eq!(map_rel_from_wiki("tests"), "tests");
    }

    #[test]
    fn rel_mapping_specific_translations() {
        assert_eq!(map_rel_to_wiki("correction"), "correction_of");
        assert_eq!(map_rel_to_wiki("refinement"), "refines");
        assert_eq!(map_rel_to_wiki("duplicate"), "duplicate_of");
        assert_eq!(map_rel_from_wiki("correction_of"), "correction");
        assert_eq!(map_rel_from_wiki("refines"), "refinement");
        assert_eq!(map_rel_from_wiki("duplicate_of"), "duplicate");
    }

    #[test]
    fn edge_type_mapping_known_and_default() {
        // heuristica di import: alias noti + default 'relates'.
        assert_eq!(map_edge_type_to_rel("depends_on"), "depends_on");
        assert_eq!(map_edge_type_to_rel("requires"), "depends_on");
        assert_eq!(map_edge_type_to_rel("needs"), "depends_on");
        assert_eq!(map_edge_type_to_rel("refinement"), "refines");
        assert_eq!(map_edge_type_to_rel("qualcosa_di_ignoto"), "relates");
    }
}
