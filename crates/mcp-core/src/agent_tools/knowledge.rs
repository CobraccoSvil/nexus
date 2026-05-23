//! MCP tools per la Knowledge Base per-progetto.
//!
//! Tre tool esposti all'agente:
//!   - `knowledge_search`: cerca note simili al query nel progetto corrente
//!   - `knowledge_get_note`: recupera body completo di una nota dato il suo id
//!   - `knowledge_create_note`: crea una nuova nota funzionale (feature/decision/...)
//!
//! Tutti i tool scoped al `ctx.project_id` - mai cross-project.

use super::AgentToolContext;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

/// `knowledge_search` — cerca top-K note rilevanti via embedding Qdrant.
///
/// Input:
///   - `query`: testo da cercare (obbligatorio, max 2000 char)
///   - `top_k`: numero hit massimi (default 5, clamp 1-20)
///   - `min_score`: soglia minima similarita' (default 0.4)
///
/// Output: array di {note_id, title, intent, status, tags, score, snippet}
pub async fn tool_knowledge_search(ctx: &AgentToolContext, input: &Value) -> String {
    let query = match input.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.trim().is_empty() => q.trim(),
        _ => return json!({"error": "query mancante o vuota"}).to_string(),
    };
    let top_k = input
        .get("top_k")
        .and_then(|v| v.as_u64())
        .unwrap_or(5)
        .clamp(1, 20) as usize;
    let min_score = input
        .get("min_score")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.4) as f32;

    let embed_text = if query.len() > 2000 { &query[..2000] } else { query };
    let vector = match ctx.neural.embed_text("", embed_text).await {
        Ok(v) => v,
        Err(e) => return json!({"error": format!("embed fallito: {e}")}).to_string(),
    };

    let hits = match crate::vector_memory::search_knowledge_points(
        &ctx.db,
        vector,
        ctx.project_id,
        top_k * 2,
    )
    .await
    {
        Ok(h) => h,
        Err(e) => return json!({"error": format!("search Qdrant fallita: {e}")}).to_string(),
    };

    let note_hits: Vec<(Uuid, f32)> = hits
        .iter()
        .filter(|h| (h.score as f32) >= min_score)
        .filter_map(|h| {
            h.payload
                .get("note_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<Uuid>().ok())
                .map(|id| (id, h.score as f32))
        })
        .take(top_k)
        .collect();
    if note_hits.is_empty() {
        return json!({"results": [], "message": "nessuna nota trovata sopra la soglia"}).to_string();
    }

    let ids: Vec<Uuid> = note_hits.iter().map(|(id, _)| *id).collect();
    let rows = match sqlx::query(
        r#"
        SELECT id, title, body_md, tags, intent, status
        FROM project_knowledge_notes
        WHERE id = ANY($1) AND project_id = $2 AND status IN ('active', 'draft')
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
        by_id.insert(
            id,
            json!({
                "note_id": id.to_string(),
                "title": r.try_get::<String, _>("title").unwrap_or_default(),
                "intent": r.try_get::<Option<String>, _>("intent").ok().flatten(),
                "status": r.try_get::<String, _>("status").unwrap_or_default(),
                "tags": r.try_get::<Vec<String>, _>("tags").unwrap_or_default(),
                "snippet": snippet,
                "truncated": body.len() > 300,
            }),
        );
    }

    let results: Vec<Value> = note_hits
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

/// `knowledge_get_note` — recupera il body completo di una nota.
///
/// Input: `note_id` (UUID string)
/// Output: {id, title, body_md, intent, status, tags, file_paths, created_at}
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
        SELECT id, title, body_md, intent, status, tags, file_paths, created_at, updated_at,
               source_message_id, source_run_id
        FROM project_knowledge_notes
        WHERE id = $1 AND project_id = $2
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

    // Aggiorna access_count + last_accessed_at
    let _ = sqlx::query(
        "UPDATE project_knowledge_notes SET access_count = access_count + 1, last_accessed_at = NOW() WHERE id = $1",
    )
    .bind(note_id)
    .execute(&*ctx.db)
    .await;

    json!({
        "id": note_id.to_string(),
        "title": row.try_get::<String, _>("title").unwrap_or_default(),
        "body_md": row.try_get::<String, _>("body_md").unwrap_or_default(),
        "intent": row.try_get::<Option<String>, _>("intent").ok().flatten(),
        "status": row.try_get::<String, _>("status").unwrap_or_default(),
        "tags": row.try_get::<Vec<String>, _>("tags").unwrap_or_default(),
        "file_paths": row.try_get::<Vec<String>, _>("file_paths").unwrap_or_default(),
        "source_message_id": row.try_get::<Option<Uuid>, _>("source_message_id").ok().flatten().map(|u| u.to_string()),
        "source_run_id": row.try_get::<Option<Uuid>, _>("source_run_id").ok().flatten().map(|u| u.to_string()),
    })
    .to_string()
}

/// `knowledge_create_note` — crea una nuova nota funzionale.
///
/// Input:
///   - `title`: titolo nota (obbligatorio, 1-200 char)
///   - `body_md`: corpo Markdown (obbligatorio)
///   - `intent`: feature/requirement/decision/domain/user_story/architecture/... (default "feature")
///   - `tags`: array di tag (opzionale)
///   - `file_paths`: array di file path correlati (opzionale)
///
/// Genera embedding + upsert Qdrant + SSE event + scrive vault .md.
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
    let tags: Vec<String> = input
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let file_paths: Vec<String> = input
        .get("file_paths")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let note_id = Uuid::new_v4();

    // Embedding + Qdrant upsert
    let embed_text = if body_md.len() > 2000 { &body_md[..2000] } else { &body_md };
    let qdrant_point_id = match ctx.neural.embed_text("", embed_text).await {
        Ok(vector) => {
            let point_id = Uuid::new_v4().to_string();
            let payload = json!({
                "project_id": ctx.project_id.to_string(),
                "note_id": note_id.to_string(),
                "intent": intent,
                "status": "active",
            });
            match crate::vector_memory::upsert_knowledge_point(&ctx.db, &point_id, vector, payload).await {
                Ok(_) => Some(point_id),
                Err(e) => {
                    tracing::warn!(error = %e, "knowledge_create_note: Qdrant upsert fallito");
                    None
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "knowledge_create_note: embed fallito");
            None
        }
    };

    // INSERT note row
    let result = sqlx::query(
        r#"
        INSERT INTO project_knowledge_notes
            (id, project_id, intent, title, body_md, status, qdrant_point_id, tags, file_paths)
        VALUES ($1, $2, $3, $4, $5, 'active', $6, $7, $8)
        "#,
    )
    .bind(note_id)
    .bind(ctx.project_id)
    .bind(&intent)
    .bind(&title)
    .bind(&body_md)
    .bind(qdrant_point_id.as_deref())
    .bind(&tags)
    .bind(&file_paths)
    .execute(&*ctx.db)
    .await;
    if let Err(e) = result {
        return json!({"error": format!("INSERT nota: {e}")}).to_string();
    }

    // Upsert tag aggregati
    for tag in &tags {
        let _ = sqlx::query(
            r#"
            INSERT INTO project_knowledge_tags (project_id, tag, note_count, last_used_at)
            VALUES ($1, $2, 1, NOW())
            ON CONFLICT (project_id, tag) DO UPDATE SET
                note_count = project_knowledge_tags.note_count + 1,
                last_used_at = NOW()
            "#,
        )
        .bind(ctx.project_id)
        .bind(tag)
        .execute(&*ctx.db)
        .await;
    }

    tracing::info!(
        project_id = %ctx.project_id,
        note_id = %note_id,
        intent = %intent,
        "knowledge_create_note: nota creata via MCP tool"
    );

    json!({
        "ok": true,
        "note_id": note_id.to_string(),
        "intent": intent,
        "qdrant_indexed": qdrant_point_id.is_some(),
    })
    .to_string()
}
