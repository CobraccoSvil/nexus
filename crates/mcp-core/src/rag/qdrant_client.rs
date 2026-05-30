//! Client HTTP minimo per Qdrant.
//!
//! Usa l'API REST (`PUT /collections/{c}` per ensure, `PUT /collections/{c}/points`
//! per upsert, `POST /collections/{c}/points/search` per search, `POST
//! /collections/{c}/points/delete` per delete). Non porta dipendenze in piu'
//! (riusa reqwest gia' nel workspace).
//!
//! Filtraggio nativo Qdrant via `must` su `payload.project_id` e altri campi.

use reqwest::Client;
use serde_json::{json, Value};

use super::RagError;

/// Crea la collection se non esiste (idempotente).
pub async fn ensure_collection(
    client: &Client,
    base_url: &str,
    collection: &str,
    dim: usize,
) -> Result<(), RagError> {
    // Check esistenza
    let check_url = format!("{}/collections/{}", base_url.trim_end_matches('/'), collection);
    let resp = client
        .get(&check_url)
        .send()
        .await
        .map_err(|e| RagError::Qdrant(format!("get collection: {e}")))?;
    if resp.status().is_success() {
        return Ok(());
    }

    let body = json!({
        "vectors": {
            "size": dim,
            "distance": "Cosine"
        }
    });
    let resp = client
        .put(&check_url)
        .json(&body)
        .send()
        .await
        .map_err(|e| RagError::Qdrant(format!("create collection: {e}")))?;
    if !resp.status().is_success() {
        let st = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(RagError::Qdrant(format!(
            "create collection {collection} fallita: {st} {text}"
        )));
    }
    // Crea indici payload per filtri frequenti.
    for field in &["project_id", "source_id", "source_kind", "session_id"] {
        let idx_url = format!(
            "{}/collections/{}/index",
            base_url.trim_end_matches('/'),
            collection
        );
        let _ = client
            .put(&idx_url)
            .json(&json!({"field_name": field, "field_schema": "keyword"}))
            .send()
            .await;
    }
    tracing::info!("rag: creata collection Qdrant '{}' dim={}", collection, dim);
    Ok(())
}

/// Upsert batch di punti. `points` e' array di
/// `{id: string|uuid, vector: [f32;dim], payload: {...}}`.
pub async fn upsert_points(
    client: &Client,
    base_url: &str,
    collection: &str,
    points: Vec<Value>,
) -> Result<(), RagError> {
    if points.is_empty() {
        return Ok(());
    }
    let url = format!(
        "{}/collections/{}/points?wait=true",
        base_url.trim_end_matches('/'),
        collection
    );
    let body = json!({"points": points});
    let resp = client
        .put(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| RagError::Qdrant(format!("upsert: {e}")))?;
    if !resp.status().is_success() {
        let st = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(RagError::Qdrant(format!(
            "upsert fallito: {st} {text}"
        )));
    }
    Ok(())
}

/// Search semantico filtrato. `must_filters` e' una lista di
/// `(field, value)` che vengono AND-combinati.
pub async fn search_points(
    client: &Client,
    base_url: &str,
    collection: &str,
    vector: Vec<f32>,
    top_k: usize,
    must_filters: Vec<(String, Value)>,
) -> Result<Vec<QdrantHit>, RagError> {
    let url = format!(
        "{}/collections/{}/points/search",
        base_url.trim_end_matches('/'),
        collection
    );
    let must_array: Vec<Value> = must_filters
        .into_iter()
        .map(|(k, v)| json!({"key": k, "match": {"value": v}}))
        .collect();
    let mut body = json!({
        "vector": vector,
        "limit": top_k,
        "with_payload": true,
    });
    if !must_array.is_empty() {
        body["filter"] = json!({"must": must_array});
    }
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| RagError::Qdrant(format!("search: {e}")))?;
    if !resp.status().is_success() {
        let st = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(RagError::Qdrant(format!("search fallita: {st} {text}")));
    }
    let parsed: Value = resp
        .json()
        .await
        .map_err(|e| RagError::Qdrant(format!("parse search: {e}")))?;
    let arr = parsed
        .get("result")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let id = item
            .get("id")
            .map(|v| v.to_string().trim_matches('"').to_string())
            .unwrap_or_default();
        let score = item.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let payload = item.get("payload").cloned().unwrap_or(Value::Null);
        out.push(QdrantHit { id, score, payload });
    }
    Ok(out)
}

/// Cancella tutti i punti che matchano i filtri (es. tutto un source_id).
pub async fn delete_by_filter(
    client: &Client,
    base_url: &str,
    collection: &str,
    must_filters: Vec<(String, Value)>,
) -> Result<(), RagError> {
    if must_filters.is_empty() {
        return Err(RagError::Qdrant("delete senza filtri rifiutata".into()));
    }
    let url = format!(
        "{}/collections/{}/points/delete?wait=true",
        base_url.trim_end_matches('/'),
        collection
    );
    let must_array: Vec<Value> = must_filters
        .into_iter()
        .map(|(k, v)| json!({"key": k, "match": {"value": v}}))
        .collect();
    let body = json!({"filter": {"must": must_array}});
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| RagError::Qdrant(format!("delete: {e}")))?;
    if !resp.status().is_success() {
        let st = resp.status();
        let text = resp.text().await.unwrap_or_default();
        // 404 collection inesistente: non e' un errore (cleanup idempotente).
        if st.as_u16() == 404 {
            return Ok(());
        }
        return Err(RagError::Qdrant(format!("delete fallita: {st} {text}")));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct QdrantHit {
    pub id: String,
    pub score: f32,
    pub payload: Value,
}
