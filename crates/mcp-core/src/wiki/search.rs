// ═══════════════════════════════════════════════════════════════════════════
// wiki/search.rs — Ricerca semantica unificata `/api/wiki/search` (ADR 0017 v2).
//
// Flusso:
//   1. Embed del testo `q` (riusa `state.orchestrator.neural.embed_text`).
//   2. Costruisce un filtro Qdrant sui payload (`scope`, `project_id`,
//      `kind`, `tags`) coerente con i parametri della richiesta.
//   3. Interroga `search_wiki_content_points_filtered` (vector_memory) con
//      `score_threshold = min_score` e `top_k = limit * 2` per dare margine
//      al filtro ACL.
//   4. Risolve gli hit -> Postgres (`wiki_docs`) JOIN con ACL clause:
//      qualunque hit Qdrant non visibile all'utente viene scartato qui.
//      Per gli utenti non-admin con `include_cross_scope=true` su
//      `scope=project` includiamo anche i meta-doc con `public_read=true`.
//   5. Genera snippet del body (200 char attorno al primo match `q`,
//      altrimenti i primi 200 char).
//
// Niente nomi modello hardcoded (regola G): l'embedder lo decide il brain a
// monte. Niente fallback su collection legacy: se Qdrant non risponde, l'API
// restituisce 503-style con messaggio chiaro.
// ═══════════════════════════════════════════════════════════════════════════

use crate::auth::Claims;
use crate::wiki::acl::WikiAcl;
use crate::wiki::model::{WikiDoc, WikiScope};
use crate::AppState;
use axum::{extract::State, http::StatusCode, Extension, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct SearchBody {
    pub q: String,
    /// `meta` | `project` | `all` (default `all`).
    pub scope: Option<String>,
    pub project_id: Option<Uuid>,
    pub limit: Option<i64>,
    pub min_score: Option<f64>,
    /// Se true, su `scope=project` include anche i meta-doc con `public_read=true`.
    /// Default false.
    pub include_cross_scope: Option<bool>,
    pub filter: Option<SearchFilter>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SearchFilter {
    pub kind: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
}

fn err500<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}"))
}
fn err400<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, format!("{e}"))
}

/// Costruisce il filtro Qdrant a partire dalla richiesta.
/// I match sono `must` (AND); per gli scope multipli (es. cross-scope) usiamo
/// `should` annidato dentro un `must` separato.
fn build_qdrant_filter(
    scope: Option<WikiScope>,
    project_id: Option<Uuid>,
    include_cross_scope: bool,
    filter: &SearchFilter,
) -> Option<Value> {
    let mut must: Vec<Value> = Vec::new();

    // Scope: se richiesto progetto e cross-scope, ammettiamo meta+project; se
    // solo scope=project senza cross-scope, ammettiamo solo project; idem per
    // scope=meta (cross-scope ignorato per scope=meta perche' non ha senso).
    match scope {
        Some(WikiScope::Meta) => {
            must.push(json!({
                "key": "scope",
                "match": { "value": "meta" }
            }));
        }
        Some(WikiScope::Project) => {
            if include_cross_scope {
                must.push(json!({
                    "should": [
                        { "key": "scope", "match": { "value": "project" } },
                        { "key": "scope", "match": { "value": "meta" } },
                    ]
                }));
            } else {
                must.push(json!({
                    "key": "scope",
                    "match": { "value": "project" }
                }));
            }
        }
        None => {}
    }

    if let Some(pid) = project_id {
        // Quando include_cross_scope=true il match esatto su project_id
        // escluderebbe i meta-doc (che hanno project_id null). Per cui in
        // quella modalita' usiamo `should`: o e' del progetto, o e' meta.
        if include_cross_scope && matches!(scope, Some(WikiScope::Project)) {
            must.push(json!({
                "should": [
                    { "key": "project_id", "match": { "value": pid.to_string() } },
                    { "key": "scope", "match": { "value": "meta" } },
                ]
            }));
        } else {
            must.push(json!({
                "key": "project_id",
                "match": { "value": pid.to_string() }
            }));
        }
    }

    if let Some(kinds) = filter.kind.as_ref().filter(|v| !v.is_empty()) {
        let should: Vec<Value> = kinds
            .iter()
            .map(|k| json!({ "key": "kind", "match": { "value": k } }))
            .collect();
        must.push(json!({ "should": should }));
    }

    if let Some(tags) = filter.tags.as_ref().filter(|v| !v.is_empty()) {
        // Qdrant match su array payload: usiamo `match: { value }` su `tags`,
        // Qdrant considera l'array come "qualunque elemento corrisponde".
        // Tutti i tag richiesti devono comparire (AND).
        for t in tags {
            must.push(json!({
                "key": "tags",
                "match": { "value": t }
            }));
        }
    }

    if must.is_empty() {
        None
    } else {
        Some(json!({ "must": must }))
    }
}

/// Estrae uno snippet dal `body_md` (200 char attorno al primo match
/// case-insensitive di `q`, o i primi 200 char). Niente HTML, solo testo.
fn make_snippet(body: &str, q: &str) -> String {
    let max = 240usize;
    let q_lc = q.to_ascii_lowercase();
    let body_lc = body.to_ascii_lowercase();
    if let Some(pos) = body_lc.find(&q_lc) {
        // Centra lo snippet attorno al match.
        let half = max / 2;
        let start = pos.saturating_sub(half);
        let end = (pos + q.len() + half).min(body.len());
        // Allinea ai char boundary (multi-byte safe).
        let start = (start..=pos)
            .find(|i| body.is_char_boundary(*i))
            .unwrap_or(0);
        let end = (pos..=end)
            .rev()
            .find(|i| body.is_char_boundary(*i))
            .unwrap_or(body.len());
        let mut s = String::new();
        if start > 0 {
            s.push_str("...");
        }
        s.push_str(&body[start..end]);
        if end < body.len() {
            s.push_str("...");
        }
        s
    } else {
        let end = body
            .char_indices()
            .nth(max)
            .map(|(i, _)| i)
            .unwrap_or_else(|| body.len());
        body[..end].to_string()
    }
}

/// `POST /api/wiki/search`
pub async fn search(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<SearchBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let acl = WikiAcl::from_claims(&state, &claims)
        .await
        .map_err(err500)?;

    let q = body.q.trim();
    if q.is_empty() {
        return Err(err400("query 'q' vuota"));
    }
    let scope: Option<WikiScope> = match body.scope.as_deref() {
        None | Some("") | Some("all") => None,
        Some(raw) => Some(WikiScope::parse(raw).ok_or_else(|| err400("scope invalido"))?),
    };
    let limit = body.limit.unwrap_or(20).clamp(1, 100);
    let min_score = body.min_score.unwrap_or(0.55).clamp(0.0, 1.0);
    let include_cross_scope = body.include_cross_scope.unwrap_or(false);
    let filter = body.filter.unwrap_or_default();

    // ── Embed ─────────────────────────────────────────────────────────────
    let embed_text = if q.len() > 2000 { &q[..2000] } else { q };
    let vector = state
        .orchestrator
        .neural
        .embed_text("", embed_text)
        .await
        .map_err(|e| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("embed fallito: {e}"),
            )
        })?;

    // ── Qdrant search con filtro payload ──────────────────────────────────
    let qfilter = build_qdrant_filter(scope, body.project_id, include_cross_scope, &filter);
    // top_k margine: chiediamo `limit * 3` per dare spazio al filtro ACL
    // (qualche hit potrebbe non essere visibile e va scartato).
    let top_k_qdrant = ((limit as usize).saturating_mul(3)).clamp(10, 300);

    let hits = crate::vector_memory::search_wiki_content_points_filtered(
        &state.db,
        vector,
        top_k_qdrant,
        min_score,
        qfilter,
    )
    .await
    .map_err(|e| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("qdrant search: {e}"),
        )
    })?;

    if hits.is_empty() {
        return Ok(Json(json!({
            "results": Vec::<Value>::new(),
            "total_returned": 0,
        })));
    }

    // ── Risolve hit -> Postgres con ACL clause ────────────────────────────
    let ids: Vec<Uuid> = hits
        .iter()
        .filter_map(|h| h.point_id.parse::<Uuid>().ok())
        .collect();
    if ids.is_empty() {
        return Ok(Json(json!({
            "results": Vec::<Value>::new(),
            "total_returned": 0,
        })));
    }

    let (acl_clause, acl_projects) = acl.scope_clause(1);
    let acl_param_used = !acl_projects.is_empty();
    // L'ids placeholder e' $2 se ACL bind usato, altrimenti $1.
    let ids_idx = if acl_param_used { 2 } else { 1 };

    let sql = format!(
        "SELECT * FROM wiki_docs WHERE {} AND wiki_docs.id = ANY(${}::uuid[])",
        acl_clause, ids_idx,
    );
    let mut query = sqlx::query_as::<_, WikiDoc>(&sql);
    if acl_param_used {
        query = query.bind(acl_projects.clone());
    }
    query = query.bind(&ids);
    let docs = query.fetch_all(&state.db).await.map_err(err500)?;

    // ── Re-order by score (hits e' gia' in ordine di score) ───────────────
    let mut doc_by_id: std::collections::HashMap<Uuid, WikiDoc> =
        docs.into_iter().map(|d| (d.id, d)).collect();

    let mut results: Vec<Value> = Vec::with_capacity(limit as usize);
    for hit in hits.into_iter() {
        if results.len() >= limit as usize {
            break;
        }
        let Some(id) = hit.point_id.parse::<Uuid>().ok() else {
            continue;
        };
        let Some(doc) = doc_by_id.remove(&id) else {
            // Hit non visibile per ACL -> skip silenzioso.
            continue;
        };
        let snippet = make_snippet(&doc.body_md, q);
        results.push(json!({
            "doc": doc,
            "score": hit.score,
            "snippet": snippet,
        }));
    }

    let total_returned = results.len();
    Ok(Json(json!({
        "results": results,
        "total_returned": total_returned,
        "filters": {
            "scope": scope.map(|s| s.as_str()),
            "project_id": body.project_id,
            "limit": limit,
            "min_score": min_score,
            "include_cross_scope": include_cross_scope,
        }
    })))
}
