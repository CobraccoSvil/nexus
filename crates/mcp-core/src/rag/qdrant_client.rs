//! Client HTTP minimo per Qdrant.
//!
//! Usa l'API REST (`PUT /collections/{c}` per ensure, `PUT /collections/{c}/points`
//! per upsert, `POST /collections/{c}/points/search` per search). Non porta
//! dipendenze in piu' (riusa reqwest gia' nel workspace).
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
    let check_url = format!(
        "{}/collections/{}",
        base_url.trim_end_matches('/'),
        collection
    );
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
        return Err(RagError::Qdrant(format!("upsert fallito: {st} {text}")));
    }
    Ok(())
}

/// Esito di una interrogazione a una collection.
///
/// «La collection non esiste» NON e' un guasto e non e' zero risultati: e' un
/// fatto di configurazione permanente, che un `Err` opaco e un `Vec` vuoto
/// rendevano entrambi indistinguibili da «cercato e non trovato» (regola Q:
/// l'ignoto e' una variante, non un valore comodo). Da qui in poi il chiamante
/// ha un CAMPO su cui decidere invece di una stringa da leggere.
#[derive(Debug)]
pub enum EsitoRicerca {
    Hits(Vec<QdrantHit>),
    /// La collection non esiste su questo Qdrant.
    CollectionAssente,
}

/// Come si classifica la risposta di Qdrant a una search.
///
/// Il segnale e' lo STATUS CODE, mai il testo del corpo (regola M): Qdrant
/// risponde `404` con `{"status":{"error":"Not found: Collection ... doesn't
/// exist!"}}`, e quel messaggio cambia con la versione mentre lo status no. La
/// rotta la costruiamo noi, quindi su questo endpoint un 404 puo' significare
/// solo «quella collection non c'e'».
pub(crate) fn esito_da_status(status: reqwest::StatusCode) -> Option<EsitoRicerca> {
    match status {
        s if s.is_success() => None,
        reqwest::StatusCode::NOT_FOUND => Some(EsitoRicerca::CollectionAssente),
        _ => None,
    }
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
) -> Result<EsitoRicerca, RagError> {
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
    if let Some(esito) = esito_da_status(resp.status()) {
        return Ok(esito);
    }
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
        let score = item.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let payload = item.get("payload").cloned().unwrap_or(Value::Null);
        out.push(QdrantHit { score, payload });
    }
    Ok(EsitoRicerca::Hits(out))
}

#[derive(Debug, Clone)]
pub struct QdrantHit {
    pub score: f32,
    pub payload: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La classificazione guarda lo STATUS, non il corpo. Il 404 e' l'unico
    /// codice che dichiara l'assenza della collection; un 500 o un 400 sono
    /// guasti e restano tali (un `Err`), perche' trattarli come «assente»
    /// direbbe all'operatore di ricreare una collection che c'e'.
    #[test]
    fn solo_il_404_dichiara_la_collection_assente() {
        assert!(matches!(
            esito_da_status(reqwest::StatusCode::NOT_FOUND),
            Some(EsitoRicerca::CollectionAssente)
        ));
        for altro in [
            reqwest::StatusCode::OK,
            reqwest::StatusCode::BAD_REQUEST,
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert!(
                esito_da_status(altro).is_none(),
                "{altro} non dichiara un'assenza"
            );
        }
    }
}
