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

use super::AgentToolContext;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

/// rel_type esposti agli agenti (schema stabile). Lo stesso set della vecchia
/// `knowledge.rel_type`: gli agenti gia' deployati ne dipendono.
pub(crate) const KNOWLEDGE_REL_TYPES: [&str; 7] = [
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

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_search
// ═══════════════════════════════════════════════════════════════════════════

/// `knowledge_search` — top-K doc rilevanti via embedding Qdrant.
///
/// Input: { query, top_k?=5 (1..=100), min_score?=0.4 }.
/// Output: { results: [{note_id,title,intent,status,tags,score,snippet}], count }
/// oppure (top_k > soglia) { mode:"summary", clusters:[{theme,count,sample_titles}] }.
pub async fn tool_knowledge_search(ctx: &AgentToolContext, input: &Value) -> String {
    let query = match input.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.trim().is_empty() => q.trim(),
        _ => return json!({"error": "query mancante o vuota"}).to_string(),
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

    // Soglia summary-mode (DB-driven, regola G — niente fallback hardcoded
    // sopra il safe default 20).
    let summary_threshold: usize = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'agent.kb.graph_summary_threshold_topk'",
    )
    .fetch_optional(&*ctx.db)
    .await
    .ok()
    .flatten()
    .and_then(|v| v.trim().parse().ok())
    .unwrap_or(20);
    let summary_mode = top_k > summary_threshold;

    if summary_mode {
        // Cluster per `intent` (o `kind` se intent assente) sui doc del progetto.
        // Esclude i doc 'frozen' (semantica equivalente al vecchio off_topic).
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
            Err(e) => return json!({"error": format!("DB cluster query: {e}")}).to_string(),
        };
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
        return json!({
            "mode": "summary",
            "clusters": clusters,
            "total": total,
            "hint": "Per body completo di un cluster: knowledge_search(query, top_k<=20)."
        })
        .to_string();
    }

    let embed_text = if query.len() > 2000 {
        &query[..2000]
    } else {
        query
    };
    let vector = match ctx.neural.embed_text("", embed_text).await {
        Ok(v) => v,
        Err(e) => return json!({"error": format!("embed fallito: {e}")}).to_string(),
    };

    // Filtro Qdrant: scope=project AND project_id=ctx.project_id.
    let qfilter = json!({
        "must": [
            { "key": "scope", "match": { "value": "project" } },
            { "key": "project_id", "match": { "value": ctx.project_id.to_string() } }
        ]
    });

    let hits = match crate::vector_memory::search_wiki_content_points_filtered(
        &ctx.db,
        vector,
        (top_k * 2).max(10),
        min_score as f64,
        Some(qfilter),
    )
    .await
    {
        Ok(h) => h,
        Err(e) => return json!({"error": format!("search Qdrant fallita: {e}")}).to_string(),
    };

    let doc_hits: Vec<(Uuid, f32)> = hits
        .iter()
        .filter(|h| (h.score as f32) >= min_score)
        .filter_map(|h| {
            h.point_id
                .parse::<Uuid>()
                .ok()
                .map(|id| (id, h.score as f32))
        })
        .take(top_k)
        .collect();
    if doc_hits.is_empty() {
        return json!({"results": [], "message": "nessun documento trovato sopra la soglia"})
            .to_string();
    }

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
        Err(e) => return json!({"error": format!("DB query: {e}")}).to_string(),
    };

    let mut by_id: std::collections::HashMap<Uuid, Value> = std::collections::HashMap::new();
    for r in &rows {
        let id: Uuid = match r.try_get("id") {
            Ok(v) => v,
            Err(_) => continue,
        };
        let body: String = r.try_get("body_md").unwrap_or_default();
        let snippet = body.chars().take(300).collect::<String>();
        // `status` esposto come "active" sempre (i frozen sono gia' filtrati);
        // manteniamo il campo per non rompere il contratto del tool.
        by_id.insert(
            id,
            json!({
                "note_id": id.to_string(),
                "title": r.try_get::<String, _>("title").unwrap_or_default(),
                "intent": r.try_get::<Option<String>, _>("intent").ok().flatten()
                    .or_else(|| r.try_get::<String, _>("kind").ok()),
                "status": "active",
                "tags": r.try_get::<Vec<String>, _>("tags").unwrap_or_default(),
                "snippet": snippet,
                "truncated": body.len() > 300,
            }),
        );
    }

    let results: Vec<Value> = doc_hits
        .iter()
        .filter_map(|(id, score)| {
            by_id.get(id).map(|note| {
                let mut n = note.clone();
                n["score"] = json!(*score);
                n
            })
        })
        .collect();

    json!({"results": results, "count": results.len()}).to_string()
}

// ═══════════════════════════════════════════════════════════════════════════
// code_doc
// ═══════════════════════════════════════════════════════════════════════════

/// `code_doc` — documentazione code-wiki di un file. Cerca doc con
/// `kind='code_doc'` il cui `vault_file_path` o `title` matcha `file_path`.
pub async fn tool_code_doc(ctx: &AgentToolContext, input: &Value) -> String {
    let file_path = match input.get("file_path").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.trim(),
        _ => return json!({"error": "file_path mancante o vuoto"}).to_string(),
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
        Err(e) => json!({ "error": format!("query fallita: {e}") }).to_string(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_get_note
// ═══════════════════════════════════════════════════════════════════════════

/// `knowledge_get_note` — body completo di un doc by id (scoped al progetto).
pub async fn tool_knowledge_get_note(ctx: &AgentToolContext, input: &Value) -> String {
    let note_id = match input
        .get("note_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(id) => id,
        None => return json!({"error": "note_id mancante o non UUID valido"}).to_string(),
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
        Ok(None) => return json!({"error": "nota non trovata o non accessibile"}).to_string(),
        Err(e) => return json!({"error": format!("DB: {e}")}).to_string(),
    };

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
    .to_string()
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_create_note
// ═══════════════════════════════════════════════════════════════════════════

/// `knowledge_create_note` — crea un doc scope=project + embedding Qdrant.
pub async fn tool_knowledge_create_note(ctx: &AgentToolContext, input: &Value) -> String {
    let title = match input
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 200)
    {
        Some(t) => t.to_string(),
        None => return json!({"error": "title mancante o invalido (1-200 char)"}).to_string(),
    };
    let body_md = match input
        .get("body_md")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(b) => b.to_string(),
        None => return json!({"error": "body_md mancante"}).to_string(),
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
    // file_paths -> tag con prefisso "file:" (preserva info nel nuovo schema).
    if let Some(arr) = input.get("file_paths").and_then(|v| v.as_array()) {
        for v in arr {
            if let Some(s) = v.as_str() {
                if !s.is_empty() {
                    tags.push(format!("file:{s}"));
                }
            }
        }
    }

    // Slug derivato dal title (slugify minimal: lowercase + replace).
    let slug = crate::wiki::vault::slugify(&title);
    if slug.is_empty() {
        return json!({"error": "title non genera slug valido"}).to_string();
    }
    let body_hash = crate::wiki::vault::sha256_hex(&body_md);
    // kind = 'note' fisso; intent porta la categoria semantica.
    let kind = "note";

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
    .bind(&slug)
    .bind(&title)
    .bind(&body_md)
    .bind(&body_hash)
    .bind(kind)
    .bind(&intent)
    .bind(&tags)
    .fetch_one(&*ctx.db)
    .await;

    let note_id = match doc_row {
        Ok(id) => id,
        Err(e) => return json!({"error": format!("INSERT wiki_docs: {e}")}).to_string(),
    };

    // Embedding + upsert Qdrant (best-effort).
    let snippet = if body_md.len() > 2000 {
        &body_md[..2000]
    } else {
        &body_md
    };
    let combined = format!("{title}\n\n{snippet}");
    let qdrant_indexed = match ctx.neural.embed_text("", &combined).await {
        Ok(vector) => {
            let point_id = note_id.to_string();
            let payload = json!({
                "scope": "project",
                "doc_id": point_id,
                "project_id": ctx.project_id.to_string(),
                "title": title,
                "tags": tags,
                "kind": kind,
                "intent": intent,
            });
            match crate::vector_memory::upsert_wiki_content_point(
                &ctx.db, &point_id, vector, payload,
            )
            .await
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
        Err(e) => {
            tracing::warn!(error = %e, "knowledge_create_note: embed fallito");
            false
        }
    };

    tracing::info!(
        project_id = %ctx.project_id,
        note_id = %note_id,
        intent = %intent,
        "knowledge_create_note: doc creato via MCP tool (wiki_docs)"
    );

    json!({
        "ok": true,
        "note_id": note_id.to_string(),
        "intent": intent,
        "qdrant_indexed": qdrant_indexed,
    })
    .to_string()
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_get_links
// ═══════════════════════════════════════════════════════════════════════════

/// `knowledge_get_links` — outbound + inbound links di un doc, scoped al progetto.
pub async fn tool_knowledge_get_links(ctx: &AgentToolContext, input: &Value) -> String {
    let note_id = match input
        .get("note_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(id) => id,
        None => return json!({"error": "note_id mancante o non UUID valido"}).to_string(),
    };

    let in_proj = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM wiki_docs \
         WHERE id = $1 AND scope = 'project' AND project_id = $2",
    )
    .bind(note_id)
    .bind(ctx.project_id)
    .fetch_one(&*ctx.db)
    .await
    .unwrap_or(0);
    if in_proj == 0 {
        return json!({"error": "nota non trovata nel progetto corrente"}).to_string();
    }

    // Outbound: edges da `note_id` verso doc visibili al progetto (proprio progetto
    // o meta public_read=true).
    let outgoing = sqlx::query(
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
        "#,
    )
    .bind(note_id)
    .bind(ctx.project_id)
    .fetch_all(&*ctx.db)
    .await
    .unwrap_or_default();

    let incoming = sqlx::query(
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
        "#,
    )
    .bind(note_id)
    .bind(ctx.project_id)
    .fetch_all(&*ctx.db)
    .await
    .unwrap_or_default();

    let to_json = |rows: &[sqlx::postgres::PgRow]| -> Vec<Value> {
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
    };

    let out = to_json(&outgoing);
    let inc = to_json(&incoming);
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

/// `knowledge_get_subgraph` — BFS dal seed (query semantica o note_id) sui link.
pub async fn tool_knowledge_get_subgraph(ctx: &AgentToolContext, input: &Value) -> String {
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

    // Mappa a vocabolario wiki_links per la query.
    let rel_filter_wiki: Vec<String> = rel_filter_input
        .iter()
        .map(|r| map_rel_to_wiki(r).to_string())
        .collect();

    // ── 1) Seed nodes ────────────────────────────────────────────────────
    let mut nodes: Vec<Uuid> = Vec::new();
    if let Some(q) = input
        .get("query")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let embed_text = if q.len() > 2000 { &q[..2000] } else { q };
        let vector = match ctx.neural.embed_text("", embed_text).await {
            Ok(v) => v,
            Err(e) => return json!({"error": format!("embed fallito: {e}")}).to_string(),
        };
        let qfilter = json!({
            "must": [
                { "key": "scope", "match": { "value": "project" } },
                { "key": "project_id", "match": { "value": ctx.project_id.to_string() } }
            ]
        });
        let hits = crate::vector_memory::search_wiki_content_points_filtered(
            &ctx.db,
            vector,
            max_nodes,
            0.0,
            Some(qfilter),
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
        return json!({"error": "serve 'query' (testo) oppure 'note_id' (UUID) come seed"})
            .to_string();
    }
    if nodes.is_empty() {
        return json!({"nodes": [], "edges": [], "message": "nessun nodo seed trovato"})
            .to_string();
    }

    // ── 2) BFS via wiki_links ────────────────────────────────────────────
    let mut frontier = nodes.clone();
    for _ in 0..depth {
        if nodes.len() >= max_nodes {
            break;
        }
        let neigh = sqlx::query(
            r#"
            SELECT from_doc_id, to_doc_id FROM wiki_links
            WHERE rel_type = ANY($1)
              AND (from_doc_id = ANY($2) OR to_doc_id = ANY($2))
            "#,
        )
        .bind(&rel_filter_wiki)
        .bind(&frontier)
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
            if nodes.len() < max_nodes {
                nodes.push(*id);
            }
        }
        frontier = next;
    }

    // ── 3) Dettagli nodi (filtra scope=project + project_id + non-frozen) ─
    let rows = sqlx::query(
        r#"
        SELECT id, title, intent, kind, edit_lock FROM wiki_docs
        WHERE id = ANY($1) AND scope = 'project' AND project_id = $2
          AND edit_lock <> 'frozen'
        "#,
    )
    .bind(&nodes)
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

    // ── 4) Archi intra-sottografo ────────────────────────────────────────
    let edges = sqlx::query(
        r#"
        SELECT from_doc_id, to_doc_id, rel_type, confidence FROM wiki_links
        WHERE rel_type = ANY($1)
          AND from_doc_id = ANY($2) AND to_doc_id = ANY($2)
        "#,
    )
    .bind(&rel_filter_wiki)
    .bind(&valid_ids)
    .fetch_all(&*ctx.db)
    .await
    .unwrap_or_default();
    let edge_json: Vec<Value> = edges
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
        .collect();

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

/// `knowledge_create_link` — crea o aggiorna un link tra due doc del progetto.
pub async fn tool_knowledge_create_link(ctx: &AgentToolContext, input: &Value) -> String {
    let from = match input
        .get("from_note_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(id) => id,
        None => return json!({"error": "from_note_id mancante o non UUID valido"}).to_string(),
    };
    let to = match input
        .get("to_note_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(id) => id,
        None => return json!({"error": "to_note_id mancante o non UUID valido"}).to_string(),
    };
    if from == to {
        return json!({"error": "self-link non ammesso (from == to)"}).to_string();
    }
    let rel_input = match input.get("rel_type").and_then(|v| v.as_str()) {
        Some(r) if KNOWLEDGE_REL_TYPES.contains(&r) => r.to_string(),
        Some(r) => {
            return json!({"error": format!("rel_type '{r}' non valido; ammessi: {KNOWLEDGE_REL_TYPES:?}")})
                .to_string()
        }
        None => return json!({"error": "rel_type mancante"}).to_string(),
    };
    let rel_wiki = map_rel_to_wiki(&rel_input);
    let confidence = input
        .get("confidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0)
        .clamp(0.0, 1.0) as f32;

    // Verifica che entrambi i doc esistano e siano accessibili dal progetto
    // (entrambi project corrente, oppure to_note appartiene a meta public).
    let cnt = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM wiki_docs \
         WHERE id = ANY($1) \
           AND ( (scope='project' AND project_id = $2) \
                 OR (scope='meta' AND public_read = TRUE) )",
    )
    .bind(vec![from, to])
    .bind(ctx.project_id)
    .fetch_one(&*ctx.db)
    .await
    .unwrap_or(0);
    if cnt != 2 {
        return json!({"error": "una o entrambe le note non esistono nel progetto corrente"})
            .to_string();
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
    .bind(from)
    .bind(to)
    .bind(rel_wiki)
    .bind(confidence)
    .execute(&*ctx.db)
    .await;

    match res {
        Ok(_) => json!({
            "ok": true,
            "from_note_id": from.to_string(),
            "to_note_id": to.to_string(),
            "rel_type": rel_input,
            "rel_type_raw": rel_wiki,
        })
        .to_string(),
        Err(e) => json!({"error": format!("INSERT link: {e}")}).to_string(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_set_relevance
// ═══════════════════════════════════════════════════════════════════════════

/// `knowledge_set_relevance` — marca un doc come off-topic (`edit_lock='frozen'`)
/// o on-topic (`edit_lock='none'`). Il campo `relevance_score` non e' piu'
/// persistito nel nuovo schema; viene accettato per compatibilita' ma ignorato.
pub async fn tool_knowledge_set_relevance(ctx: &AgentToolContext, input: &Value) -> String {
    let note_id = match input
        .get("note_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(id) => id,
        None => return json!({"error": "note_id mancante o non UUID valido"}).to_string(),
    };
    let off_topic = match input.get("off_topic").and_then(|v| v.as_bool()) {
        Some(b) => b,
        None => return json!({"error": "off_topic (bool) mancante"}).to_string(),
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
        Ok(_) => json!({"error": "nota non trovata nel progetto corrente"}).to_string(),
        Err(e) => json!({"error": format!("UPDATE relevance: {e}")}).to_string(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// knowledge_import_graph
// ═══════════════════════════════════════════════════════════════════════════

/// `knowledge_import_graph` — import grafo esterno (JSON/Mermaid/DOT) nella KB.
/// Nodi -> `wiki_docs` (scope=project), archi -> `wiki_links`.
/// Il parser di grafi `knowledge::graph_import` e' stato rimosso assieme al
/// modulo `knowledge/`; per ora supportiamo solo il formato JSON node-link
/// (`{"nodes":[{id,label,content?,node_type?}], "edges":[{source,target,type?}]}`).
pub async fn tool_knowledge_import_graph(ctx: &AgentToolContext, input: &Value) -> String {
    let format = match input.get("format").and_then(|v| v.as_str()) {
        Some(f) if !f.trim().is_empty() => f.trim().to_lowercase(),
        _ => {
            return json!({"error": "parametro 'format' obbligatorio (json | mermaid | dot)"})
                .to_string()
        }
    };
    let content = match input.get("content").and_then(|v| v.as_str()) {
        Some(c) if !c.trim().is_empty() => c.to_string(),
        _ => return json!({"error": "parametro 'content' obbligatorio"}).to_string(),
    };
    let source_id = input
        .get("source_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("import")
        .to_string();

    // Settings (riusa le chiavi storiche; safe defaults se mancanti).
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
    if !enabled {
        return json!({"error": "import grafi disabilitato (knowledge.graph_import_enabled=false)"})
            .to_string();
    }
    if format != "json" {
        return json!({
            "error": format!(
                "formato '{format}' non supportato in questa versione (solo 'json' node-link). \
                 Mermaid/DOT richiedono il parser legacy `knowledge::graph_import` non ancora portato."
            )
        })
        .to_string();
    }

    // Parsing JSON node-link minimo.
    let payload: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => return json!({"error": format!("JSON invalido: {e}")}).to_string(),
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
        return json!({"error": "nessun nodo trovato nel grafo"}).to_string();
    }
    if nodes_in.len() > max_nodes {
        return json!({"error": format!("troppi nodi: {} > max {}", nodes_in.len(), max_nodes)})
            .to_string();
    }

    let mut id_map: std::collections::HashMap<String, Uuid> = std::collections::HashMap::new();
    let mut nodes_created = 0usize;

    for n in &nodes_in {
        let ext_id = n
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if ext_id.is_empty() {
            continue;
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
        let node_type = n
            .get("node_type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let mut tags: Vec<String> = Vec::new();
        if !node_type.is_empty() {
            tags.push(node_type.clone());
        }
        tags.push(format!("ext:{source_id}"));

        // Slug stabile: includes ext_id per evitare collisioni.
        let raw_slug = format!("imp-{}-{}", source_id, ext_id);
        let slug = crate::wiki::vault::slugify(&raw_slug);
        let body_hash = crate::wiki::vault::sha256_hex(&body);

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
        .bind(&slug)
        .bind(&title)
        .bind(&body)
        .bind(&body_hash)
        .bind(&tags)
        .fetch_one(&*ctx.db)
        .await;
        if let Ok(id) = res {
            id_map.insert(ext_id, id);
            nodes_created += 1;
        }
    }

    let mut edges_created = 0usize;
    for e in &edges_in {
        let source = e
            .get("source")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let target = e
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if source.is_empty() || target.is_empty() {
            continue;
        }
        let (Some(&f), Some(&t)) = (id_map.get(&source), id_map.get(&target)) else {
            continue;
        };
        if f == t {
            continue;
        }
        // edge_type -> rel_type: heuristica semplice.
        let etype = e
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        let rel = match etype.as_str() {
            "depends_on" | "requires" | "needs" => "depends_on",
            "blocks" => "blocks",
            "blocked_by" => "blocked_by",
            "implements" => "implements",
            "tests" => "tests",
            "refines" | "refinement" => "refines",
            _ => "relates",
        };
        let r = sqlx::query(
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
        .await;
        if r.is_ok() {
            edges_created += 1;
        }
    }

    tracing::info!(
        project_id = %ctx.project_id,
        format = %format,
        nodes_created,
        edges_created,
        "knowledge_import_graph: grafo esterno importato in wiki_docs/wiki_links"
    );

    json!({
        "ok": true,
        "format": format,
        "nodes_created": nodes_created,
        "edges_created": edges_created,
        "source_id": source_id,
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
}
