use serde_json::{json, Value};
use uuid::Uuid;

/// Delete Qdrant points by IDs from the project_docs collection
pub async fn delete_doc_points(qdrant_url: &str, point_ids: &[String]) -> anyhow::Result<()> {
    let collection = "project_docs";
    let client = nexus_http::build_client();

    let ids: Vec<Value> = point_ids.iter().map(|id| json!(id)).collect();

    client
        .post(format!("{qdrant_url}/collections/{collection}/points/delete"))
        .json(&json!({ "points": ids }))
        .send()
        .await?;

    Ok(())
}

/// Search Qdrant for document sections matching a query vector
pub async fn search_doc_points(
    qdrant_url: &str,
    vector: &[f32],
    project_id: Uuid,
    doc_type: Option<&str>,
    limit: usize,
) -> anyhow::Result<Vec<Value>> {
    let collection = "project_docs";
    let client = nexus_http::build_client();

    let mut filter = json!({
        "must": [
            { "key": "project_id", "match": { "value": project_id.to_string() } }
        ]
    });

    if let Some(dt) = doc_type {
        if let Some(must) = filter.get_mut("must").and_then(Value::as_array_mut) {
            must.push(json!({ "key": "doc_type", "match": { "value": dt } }));
        }
    }

    let vector_f64: Vec<f64> = vector.iter().map(|v| *v as f64).collect();

    let resp = client
        .post(format!("{qdrant_url}/collections/{collection}/points/search"))
        .json(&json!({
            "vector": vector_f64,
            "filter": filter,
            "limit": limit,
            "with_payload": true,
        }))
        .send()
        .await?;

    let result: Value = resp.json().await?;
    let hits = result
        .get("result")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let results: Vec<Value> = hits
        .iter()
        .map(|h| {
            let payload = h.get("payload").cloned().unwrap_or(json!({}));
            json!({
                "score": h.get("score").and_then(Value::as_f64).unwrap_or(0.0),
                "doc_type": payload.get("doc_type").and_then(Value::as_str).unwrap_or(""),
                "section_path": payload.get("section_path").and_then(Value::as_str).unwrap_or(""),
                "section_title": payload.get("section_title").and_then(Value::as_str).unwrap_or(""),
                "version": payload.get("version").and_then(Value::as_str).unwrap_or(""),
                "text_preview": payload.get("text_preview").and_then(Value::as_str).unwrap_or(""),
            })
        })
        .collect();

    Ok(results)
}
