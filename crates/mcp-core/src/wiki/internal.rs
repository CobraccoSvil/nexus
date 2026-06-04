// ═══════════════════════════════════════════════════════════════════════════
// wiki/internal.rs — Endpoint interni NO-AUTH chiamati dal brain Python.
//
// Estratti da `knowledge/routes.rs` durante F8 (ADR 0017 v2). L'unico endpoint
// vivo e' `internal_kb_search`, gia' migrato a `wiki_docs` + `wiki_content`
// (collection Qdrant unificata). Il contratto JSON e' preservato bit-a-bit
// per non rompere il brain Python che lo consuma.
// ═══════════════════════════════════════════════════════════════════════════

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::AppState;

#[derive(Deserialize)]
pub struct InternalKbSearchBody {
    pub project_id: String,
    pub query: String,
    pub top_k: Option<usize>,
    pub min_score: Option<f32>,
}

/// `POST /api/internal/knowledge/search` (NO-AUTH, rete privata).
///
/// Cerca note di progetto via Qdrant `wiki_content` filtrando per
/// `scope=project` e `project_id` esatto. Il brain non vede mai meta-doc da
/// questo endpoint: per quello c'e' `/api/wiki/search` autenticato.
pub async fn internal_kb_search(
    State(state): State<AppState>,
    Json(body): Json<InternalKbSearchBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let project_id = Uuid::parse_str(&body.project_id)
        .map_err(|_| (StatusCode::BAD_REQUEST, "project_id non valido".to_string()))?;
    let query = body.query.trim();
    if query.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "query vuota".to_string()));
    }
    let top_k = body.top_k.unwrap_or(5).clamp(1, 20);
    let min_score = body.min_score.unwrap_or(0.4) as f64;

    let embed_text = if query.len() > 2000 {
        &query[..2000]
    } else {
        query
    };
    let vector = match state.orchestrator.neural.embed_text("", embed_text).await {
        Ok(v) => v,
        Err(e) => {
            return Ok(Json(json!({
                "results": [],
                "warning": format!("embed fallito: {e}"),
            })));
        }
    };

    let filter = json!({
        "must": [
            { "key": "scope", "match": { "value": "project" } },
            { "key": "project_id", "match": { "value": project_id.to_string() } },
        ]
    });

    let hits = match crate::vector_memory::search_wiki_content_points_filtered(
        &state.db,
        vector,
        top_k * 2,
        min_score,
        Some(filter),
    )
    .await
    {
        Ok(h) => h,
        Err(e) => {
            return Ok(Json(json!({
                "results": [],
                "warning": format!("Qdrant search fallita: {e}"),
            })));
        }
    };

    let doc_hits: Vec<(Uuid, f32)> = hits
        .iter()
        .filter_map(|h| h.point_id.parse::<Uuid>().ok().map(|id| (id, h.score as f32)))
        .take(top_k)
        .collect();
    if doc_hits.is_empty() {
        return Ok(Json(json!({"results": []})));
    }

    let ids: Vec<Uuid> = doc_hits.iter().map(|(id, _)| *id).collect();
    let rows = sqlx::query(
        r#"
        SELECT id, title, body_md, tags, intent, kind
        FROM wiki_docs
        WHERE id = ANY($1) AND scope = 'project' AND project_id = $2
        "#,
    )
    .bind(&ids)
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("query: {e}")))?;

    let mut by_id: std::collections::HashMap<Uuid, serde_json::Value> =
        std::collections::HashMap::new();
    for r in &rows {
        let id: Uuid = match r.try_get("id") {
            Ok(v) => v,
            Err(_) => continue,
        };
        let body: String = r.try_get("body_md").unwrap_or_default();
        let snippet = body.chars().take(400).collect::<String>();
        by_id.insert(
            id,
            json!({
                "note_id": id.to_string(),
                "title": r.try_get::<String, _>("title").unwrap_or_default(),
                "intent": r.try_get::<Option<String>, _>("intent").ok().flatten(),
                // `status` non esiste piu' in wiki_docs; manteniamo il campo a
                // "active" per compat brain (che lo filtra in chain).
                "status": "active",
                "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
                "tags": r.try_get::<Vec<String>, _>("tags").unwrap_or_default(),
                "snippet": snippet,
            }),
        );
    }

    let results: Vec<serde_json::Value> = doc_hits
        .iter()
        .filter_map(|(id, score)| {
            by_id.get(id).map(|note| {
                let mut n = note.clone();
                n["score"] = json!(*score);
                n
            })
        })
        .collect();

    Ok(Json(json!({"results": results, "count": results.len()})))
}
