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

/// Vectorize a document's sections into Qdrant
pub async fn vectorize_document(
    db: &sqlx::PgPool,
    qdrant_url: &str,
    _neural_url: &str,
    project_id: Uuid,
    doc_id: Uuid,
    doc_type: &str,
    version: &str,
    content: &Value,
) -> anyhow::Result<()> {
    let collection = "project_docs";
    let brain_rest_url = std::env::var("NEURAL_CORE_REST_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());
    let client = nexus_http::build_client();

    // Ensure collection exists
    let _ = client
        .put(format!("{qdrant_url}/collections/{collection}"))
        .json(&json!({
            "vectors": { "size": 384, "distance": "Cosine" }
        }))
        .send()
        .await;

    // Extract sections from content
    let sections = content
        .get("sections")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut point_ids: Vec<String> = Vec::new();

    for section in &sections {
        let section_title = section
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("");
        let section_number = section
            .get("number")
            .and_then(Value::as_str)
            .unwrap_or("");
        let section_content = section
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("");

        if section_content.trim().is_empty() {
            continue;
        }

        let text_to_embed = format!("{} {}: {}", section_number, section_title, section_content);

        // Get embedding
        let embed_resp = client
            .post(format!("{brain_rest_url}/embed"))
            .json(&json!({ "text": text_to_embed }))
            .send()
            .await?;

        let embed_result: Value = embed_resp.json().await?;
        let vector: Vec<f64> = embed_result
            .get("vector")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();

        if vector.is_empty() {
            continue;
        }

        let point_id = Uuid::new_v4().to_string();
        point_ids.push(point_id.clone());

        let text_preview = if section_content.len() > 200 {
            format!("{}...", &section_content[..200])
        } else {
            section_content.to_string()
        };

        // Upsert point
        let _ = client
            .put(format!("{qdrant_url}/collections/{collection}/points"))
            .json(&json!({
                "points": [{
                    "id": point_id,
                    "vector": vector,
                    "payload": {
                        "project_id": project_id.to_string(),
                        "document_id": doc_id.to_string(),
                        "doc_type": doc_type,
                        "section_path": section_number,
                        "section_title": section_title,
                        "version": version,
                        "text_preview": text_preview,
                    }
                }]
            }))
            .send()
            .await;
    }

    // Save point IDs back to DB
    if !point_ids.is_empty() {
        let _ = sqlx::query(
            "UPDATE project_documents SET qdrant_point_ids = $1 WHERE id = $2",
        )
        .bind(&point_ids)
        .bind(doc_id)
        .execute(db)
        .await;
    }

    tracing::info!(
        "Vectorized {} sections for doc {} ({})",
        point_ids.len(),
        doc_id,
        doc_type
    );

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
