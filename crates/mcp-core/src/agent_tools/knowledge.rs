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

    let embed_text = if query.len() > 2000 {
        &query[..2000]
    } else {
        query
    };
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
        return json!({"results": [], "message": "nessuna nota trovata sopra la soglia"})
            .to_string();
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
/// `code_doc` — documentazione (Code Wiki) di un file specifico, on-demand.
///
/// A differenza di `knowledge_search` (ricerca semantica fuzzy), recupera
/// direttamente la nota `code_doc` il cui titolo e' il path del file: scopo,
/// componenti, dipendenze e call-graph del file. Da usare quando l'agente sa
/// gia' su quale file lavora, per evitare di re-implementare o introdurre
/// errori. Match esatto sul path, con fallback su suffisso (relativo/assoluto).
///
/// Input: { file_path }. Output: { file, found, body } o suggerimento.
pub async fn tool_code_doc(ctx: &AgentToolContext, input: &Value) -> String {
    let file_path = match input.get("file_path").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.trim(),
        _ => return json!({"error": "file_path mancante o vuoto"}).to_string(),
    };

    let row = sqlx::query(
        r#"
        SELECT id, title, body_md FROM project_knowledge_notes
        WHERE project_id = $1 AND kind = 'code_doc'
          AND (title = $2 OR title LIKE $3 OR $2 LIKE '%' || title)
        ORDER BY (title = $2) DESC
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
            "message": "Nessuna documentazione (code_doc) per questo file. Prova knowledge_search per contesto correlato, oppure genera la Code Wiki del progetto."
        })
        .to_string(),
        Err(e) => json!({ "error": format!("query fallita: {e}") }).to_string(),
    }
}

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
    let embed_text = if body_md.len() > 2000 {
        &body_md[..2000]
    } else {
        &body_md
    };
    let qdrant_point_id = match ctx.neural.embed_text("", embed_text).await {
        Ok(vector) => {
            let point_id = Uuid::new_v4().to_string();
            let payload = json!({
                "project_id": ctx.project_id.to_string(),
                "note_id": note_id.to_string(),
                "intent": intent,
                "status": "active",
            });
            match crate::vector_memory::upsert_knowledge_point(&ctx.db, &point_id, vector, payload)
                .await
            {
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

    // RAG (ADR 0015): indicizza la nota anche nella collection unificata
    // `kb_chunks` per renderla cercabile via `nexus_search_semantic`.
    // Fire-and-forget: errori sono solo loggati.
    {
        let db_clone = ctx.db.clone();
        let pid = ctx.project_id;
        let nid = note_id;
        let body_clone = body_md.clone();
        let title_clone = title.clone();
        let intent_clone = intent.clone();
        tokio::spawn(async move {
            let metadata = serde_json::json!({
                "title": title_clone,
                "intent": intent_clone,
            });
            let combined = format!(
                "{title_clone}

{body_clone}"
            );
            if let Err(e) = crate::rag::index_text(
                &db_clone,
                crate::rag::SourceKind::Kb,
                &nid.to_string(),
                Some(pid),
                None,
                &combined,
                metadata,
            )
            .await
            {
                tracing::warn!("rag: indicizzazione KB note {} fallita: {}", nid, e);
            }
        });
    }

    json!({
        "ok": true,
        "note_id": note_id.to_string(),
        "intent": intent,
        "qdrant_indexed": qdrant_point_id.is_some(),
    })
    .to_string()
}

/// rel_type ammessi dal CHECK di `project_knowledge_links` (mig 0175).
/// Semantica per il coordinamento: `blocks`/`blocked_by` = dipendenze HARD
/// (A blocked_by B => B prima di A); `relates` = contesto correlato (non
/// dipendenza); `duplicate`/`correction`/`refinement`/`followup` = relazioni
/// di intake (vedi Componente 1).
pub(crate) const KNOWLEDGE_REL_TYPES: [&str; 7] = [
    "followup",
    "correction",
    "refinement",
    "duplicate",
    "blocks",
    "blocked_by",
    "relates",
];

/// `knowledge_get_links` — link entranti e uscenti di una nota.
///
/// Input: `note_id` (UUID). Output: {outgoing[], incoming[]} con rel_type,
/// created_by, confidence e titolo/intent della nota all'altro capo.
/// Note off_topic escluse. Scoped al progetto corrente.
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
        "SELECT COUNT(*) FROM project_knowledge_notes WHERE id = $1 AND project_id = $2",
    )
    .bind(note_id)
    .bind(ctx.project_id)
    .fetch_one(&*ctx.db)
    .await
    .unwrap_or(0);
    if in_proj == 0 {
        return json!({"error": "nota non trovata nel progetto corrente"}).to_string();
    }

    // project_knowledge_links non ha project_id: l'isolamento passa dal JOIN
    // sulle note (n.project_id) e dal filtro off_topic.
    let outgoing = sqlx::query(
        r#"
        SELECT l.id AS link_id, l.to_note_id AS other_id, l.rel_type, l.created_by, l.confidence,
               n.title, n.intent
        FROM project_knowledge_links l
        JOIN project_knowledge_notes n ON n.id = l.to_note_id
        WHERE l.from_note_id = $1 AND n.project_id = $2 AND n.off_topic = false
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
        SELECT l.id AS link_id, l.from_note_id AS other_id, l.rel_type, l.created_by, l.confidence,
               n.title, n.intent
        FROM project_knowledge_links l
        JOIN project_knowledge_notes n ON n.id = l.from_note_id
        WHERE l.to_note_id = $1 AND n.project_id = $2 AND n.off_topic = false
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
                Some(json!({
                    "link_id": r.try_get::<Uuid, _>("link_id").ok().map(|u| u.to_string()),
                    "note_id": other.to_string(),
                    "title": r.try_get::<String, _>("title").unwrap_or_default(),
                    "intent": r.try_get::<Option<String>, _>("intent").ok().flatten(),
                    "rel_type": r.try_get::<String, _>("rel_type").unwrap_or_default(),
                    "created_by": r.try_get::<String, _>("created_by").unwrap_or_default(),
                    "confidence": r.try_get::<f32, _>("confidence").unwrap_or(1.0),
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

/// `knowledge_get_subgraph` — sottografo del progetto da un seed (query o nota).
///
/// Input:
///   - `query`: testo seed (cerca le note rilevanti come radici) OPPURE
///   - `note_id`: UUID di una nota radice
///   - `rel_types`: filtro relazioni (default: tutte). Per le sole dipendenze
///     passare ["blocks","blocked_by"].
///   - `depth`: profondita' di espansione BFS (default 2, max 4)
///   - `max_nodes`: tetto nodi (default 30, max 100)
///
/// Output: {nodes:[{note_id,title,intent,status}], edges:[{from,to,rel_type,confidence}]}.
/// Note off_topic escluse. E' la base che alimenta il DAG (build_dependency_context).
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
    let rel_filter: Vec<String> = input
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

    // 1. Determina i nodi seed (da query semantica o da nota esplicita).
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
        let hits = crate::vector_memory::search_knowledge_points(
            &ctx.db,
            vector,
            ctx.project_id,
            max_nodes,
        )
        .await
        .unwrap_or_default();
        for h in hits.iter() {
            if let Some(id) = h
                .payload
                .get("note_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<Uuid>().ok())
            {
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

    // 2. Espansione BFS via link (project_knowledge_links non ha project_id;
    //    l'isolamento e' garantito dal filtro finale sulle note del progetto).
    let mut frontier = nodes.clone();
    for _ in 0..depth {
        if nodes.len() >= max_nodes {
            break;
        }
        let neigh = sqlx::query(
            r#"
            SELECT from_note_id, to_note_id FROM project_knowledge_links
            WHERE rel_type = ANY($1) AND (from_note_id = ANY($2) OR to_note_id = ANY($2))
            "#,
        )
        .bind(&rel_filter)
        .bind(&frontier)
        .fetch_all(&*ctx.db)
        .await
        .unwrap_or_default();
        let mut next: Vec<Uuid> = Vec::new();
        for r in &neigh {
            for col in ["from_note_id", "to_note_id"] {
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

    // 3. Dettagli nodi (filtra progetto + off_topic): i nodi validi finali.
    let rows = sqlx::query(
        r#"
        SELECT id, title, intent, status FROM project_knowledge_notes
        WHERE id = ANY($1) AND project_id = $2 AND off_topic = false
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
                "intent": r.try_get::<Option<String>, _>("intent").ok().flatten(),
                "status": r.try_get::<String, _>("status").unwrap_or_default(),
            }))
        })
        .collect();

    // 4. Archi tra i soli nodi validi (intra-sottografo).
    let edges = sqlx::query(
        r#"
        SELECT from_note_id, to_note_id, rel_type, confidence FROM project_knowledge_links
        WHERE rel_type = ANY($1) AND from_note_id = ANY($2) AND to_note_id = ANY($2)
        "#,
    )
    .bind(&rel_filter)
    .bind(&valid_ids)
    .fetch_all(&*ctx.db)
    .await
    .unwrap_or_default();
    let edge_json: Vec<Value> = edges
        .iter()
        .filter_map(|r| {
            let f = r.try_get::<Uuid, _>("from_note_id").ok()?;
            let t = r.try_get::<Uuid, _>("to_note_id").ok()?;
            Some(json!({
                "from": f.to_string(),
                "to": t.to_string(),
                "rel_type": r.try_get::<String, _>("rel_type").unwrap_or_default(),
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

/// `knowledge_create_link` — crea un link diretto tra due note (created_by='agent').
///
/// Input: `from_note_id`, `to_note_id` (UUID), `rel_type` (uno di KNOWLEDGE_REL_TYPES),
/// `confidence` (0-1, default 1.0). Idempotente sulla tripla (from,to,rel_type).
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
    let rel_type = match input.get("rel_type").and_then(|v| v.as_str()) {
        Some(r) if KNOWLEDGE_REL_TYPES.contains(&r) => r.to_string(),
        Some(r) => {
            return json!({"error": format!("rel_type '{r}' non valido; ammessi: {KNOWLEDGE_REL_TYPES:?}")})
                .to_string()
        }
        None => return json!({"error": "rel_type mancante"}).to_string(),
    };
    let confidence = input
        .get("confidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0)
        .clamp(0.0, 1.0) as f32;

    let cnt = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM project_knowledge_notes WHERE id = ANY($1) AND project_id = $2",
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

    let link_id: Result<Uuid, _> = sqlx::query_scalar(
        r#"
        INSERT INTO project_knowledge_links (from_note_id, to_note_id, rel_type, created_by, confidence)
        VALUES ($1, $2, $3, 'agent', $4)
        ON CONFLICT (from_note_id, to_note_id, rel_type)
        DO UPDATE SET confidence = EXCLUDED.confidence
        RETURNING id
        "#,
    )
    .bind(from)
    .bind(to)
    .bind(&rel_type)
    .bind(confidence)
    .fetch_one(&*ctx.db)
    .await;

    match link_id {
        Ok(id) => json!({
            "ok": true,
            "link_id": id.to_string(),
            "from_note_id": from.to_string(),
            "to_note_id": to.to_string(),
            "rel_type": rel_type,
        })
        .to_string(),
        Err(e) => json!({"error": format!("INSERT link: {e}")}).to_string(),
    }
}

/// `knowledge_set_relevance` — marca una nota come on/off-topic.
///
/// Input: `note_id` (UUID), `off_topic` (bool), `relevance_score` (0-1, opz.).
/// Una nota off_topic resta in KB ma e' esclusa da grafo/RAG/DAG.
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
    let score = input
        .get("relevance_score")
        .and_then(|v| v.as_f64())
        .map(|s| s.clamp(0.0, 1.0) as f32);

    let res = sqlx::query(
        r#"
        UPDATE project_knowledge_notes
        SET off_topic = $2, relevance_score = COALESCE($3, relevance_score)
        WHERE id = $1 AND project_id = $4
        "#,
    )
    .bind(note_id)
    .bind(off_topic)
    .bind(score)
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

async fn read_graph_import_settings(db: &sqlx::PgPool) -> (bool, usize) {
    let mut enabled = true;
    let mut max_nodes = 2000usize;
    let rows = sqlx::query(
        "SELECT key, value FROM settings WHERE key IN ('knowledge.graph_import_enabled','knowledge.graph_import_max_nodes')",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();
    for r in &rows {
        let k: String = r.try_get("key").unwrap_or_default();
        let v: String = r.try_get("value").unwrap_or_default();
        match k.as_str() {
            "knowledge.graph_import_enabled" => {
                enabled = !matches!(v.trim().to_lowercase().as_str(), "false" | "0" | "off")
            }
            "knowledge.graph_import_max_nodes" => max_nodes = v.trim().parse().unwrap_or(2000),
            _ => {}
        }
    }
    (enabled, max_nodes)
}

/// `knowledge_import_graph` — importa un grafo esterno (JSON node-link, Mermaid,
/// DOT) nella KB del progetto: i nodi diventano note (source_kind='external'),
/// gli archi diventano link (created_by='external'). I tipi di dipendenza
/// diventano relazioni `blocks`/`blocked_by` che alimentano il DAG.
pub async fn tool_knowledge_import_graph(ctx: &AgentToolContext, input: &Value) -> String {
    let format = match input.get("format").and_then(|v| v.as_str()) {
        Some(f) if !f.trim().is_empty() => f.trim().to_string(),
        _ => {
            return json!({"error": "parametro 'format' obbligatorio (json|mermaid|dot)"})
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

    let (enabled, max_nodes) = read_graph_import_settings(&ctx.db).await;
    if !enabled {
        return json!({"error": "import grafi disabilitato (knowledge.graph_import_enabled=false)"})
            .to_string();
    }

    let graph = match crate::knowledge::graph_import::parse_graph(&format, &content) {
        Ok(g) => g,
        Err(e) => return json!({"error": format!("parsing fallito: {e}")}).to_string(),
    };
    if graph.nodes.is_empty() {
        return json!({"error": "nessun nodo trovato nel grafo"}).to_string();
    }
    if graph.nodes.len() > max_nodes {
        return json!({"error": format!("troppi nodi: {} > max {}", graph.nodes.len(), max_nodes)})
            .to_string();
    }

    let mut id_map: std::collections::HashMap<String, Uuid> = std::collections::HashMap::new();
    let mut nodes_created = 0usize;

    for node in &graph.nodes {
        let note_id = Uuid::new_v4();
        let title: String = {
            let t: String = node.label.chars().take(200).collect();
            if t.trim().is_empty() {
                node.ext_id.chars().take(200).collect()
            } else {
                t
            }
        };
        let body_md = node.content.clone().unwrap_or_else(|| node.label.clone());
        let tags: Vec<String> = node.node_type.clone().into_iter().collect();

        let embed_src = format!("{title}\n{body_md}");
        let embed_text = if embed_src.len() > 2000 {
            &embed_src[..2000]
        } else {
            &embed_src
        };
        let qdrant_point_id = match ctx.neural.embed_text("", embed_text).await {
            Ok(vector) => {
                let point_id = Uuid::new_v4().to_string();
                let payload = json!({
                    "project_id": ctx.project_id.to_string(),
                    "note_id": note_id.to_string(),
                    "intent": "domain",
                    "status": "active",
                });
                match crate::vector_memory::upsert_knowledge_point(
                    &ctx.db, &point_id, vector, payload,
                )
                .await
                {
                    Ok(_) => Some(point_id),
                    Err(_) => None,
                }
            }
            Err(_) => None,
        };

        let res = sqlx::query(
            r#"
            INSERT INTO project_knowledge_notes
                (id, project_id, intent, title, body_md, status, qdrant_point_id, tags, file_paths, source_kind, external_source_id)
            VALUES ($1, $2, 'domain', $3, $4, 'active', $5, $6, $7, 'external', $8)
            "#,
        )
        .bind(note_id)
        .bind(ctx.project_id)
        .bind(&title)
        .bind(&body_md)
        .bind(qdrant_point_id.as_deref())
        .bind(&tags)
        .bind(Vec::<String>::new())
        .bind(&source_id)
        .execute(&*ctx.db)
        .await;
        if res.is_ok() {
            id_map.insert(node.ext_id.clone(), note_id);
            nodes_created += 1;
        }
    }

    let mut edges_created = 0usize;
    for edge in &graph.edges {
        if let (Some(&f), Some(&t)) = (id_map.get(&edge.source), id_map.get(&edge.target)) {
            if f == t {
                continue;
            }
            let rel = crate::knowledge::graph_import::edge_type_to_rel(edge.edge_type.as_deref());
            let r = sqlx::query(
                r#"
                INSERT INTO project_knowledge_links (from_note_id, to_note_id, rel_type, created_by, confidence)
                VALUES ($1, $2, $3, 'external', 1.0)
                ON CONFLICT (from_note_id, to_note_id, rel_type) DO NOTHING
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
    }

    tracing::info!(
        project_id = %ctx.project_id,
        format = %format,
        nodes_created,
        edges_created,
        "knowledge_import_graph: grafo esterno importato"
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
