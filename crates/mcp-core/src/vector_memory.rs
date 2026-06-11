use anyhow::{anyhow, Context};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

const DEFAULT_QDRANT_URL: &str = "http://localhost:6333";
const DEFAULT_CORRECTIONS_COLLECTION: &str = "prompt_corrections";
const DEFAULT_PROJECT_CONTEXT_COLLECTION: &str = "project_context";
const DEFAULT_CODE_INDEX_COLLECTION: &str = "project_code_index";

#[derive(Debug, Clone)]
pub struct VectorPointHit {
    pub point_id: String,
    pub score: f64,
    pub payload: Value,
}

/// Lettura setting: punto unico in nexus-auth (regola L / ADR 0026).
/// Re-export con la semantica storica (Result, trim + scarto dei vuoti).
pub(crate) use nexus_auth::get_setting_nonempty as get_setting;

async fn qdrant_config(db: &PgPool) -> anyhow::Result<(String, String)> {
    let url = get_setting(db, "qdrant_url").await?.unwrap_or_else(|| {
        std::env::var("QDRANT_URL").unwrap_or_else(|_| DEFAULT_QDRANT_URL.to_string())
    });

    let collection = get_setting(db, "qdrant_prompt_corrections_collection")
        .await?
        .or_else(|| std::env::var("QDRANT_PROMPT_CORRECTIONS_COLLECTION").ok())
        .unwrap_or_else(|| DEFAULT_CORRECTIONS_COLLECTION.to_string());

    Ok((url, collection))
}

async fn qdrant_project_context_config(db: &PgPool) -> anyhow::Result<(String, String)> {
    let url = get_setting(db, "qdrant_url").await?.unwrap_or_else(|| {
        std::env::var("QDRANT_URL").unwrap_or_else(|_| DEFAULT_QDRANT_URL.to_string())
    });

    let collection = get_setting(db, "qdrant_project_context_collection")
        .await?
        .or_else(|| std::env::var("QDRANT_PROJECT_CONTEXT_COLLECTION").ok())
        .unwrap_or_else(|| DEFAULT_PROJECT_CONTEXT_COLLECTION.to_string());

    Ok((url, collection))
}

async fn ensure_collection(db: &PgPool) -> anyhow::Result<()> {
    let (base_url, collection) = qdrant_config(db).await?;
    let client = nexus_http::build_client();
    let get_url = format!("{base_url}/collections/{collection}");

    let response = client
        .get(&get_url)
        .send()
        .await
        .context("failed to check qdrant collection")?;

    if response.status().is_success() {
        return Ok(());
    }

    let create_url = format!("{base_url}/collections/{collection}");
    let create_body = json!({
        "vectors": {
            "size": 384,
            "distance": "Cosine"
        }
    });

    let create_response = client
        .put(&create_url)
        .json(&create_body)
        .send()
        .await
        .context("failed to create qdrant collection")?;

    if !create_response.status().is_success() {
        let payload = create_response
            .text()
            .await
            .unwrap_or_else(|_| "unknown".to_string());
        return Err(anyhow!("unable to create qdrant collection: {payload}"));
    }

    Ok(())
}

async fn ensure_project_context_collection(db: &PgPool) -> anyhow::Result<()> {
    let (base_url, collection) = qdrant_project_context_config(db).await?;
    let client = nexus_http::build_client();
    let get_url = format!("{base_url}/collections/{collection}");

    let response = client
        .get(&get_url)
        .send()
        .await
        .context("failed to check project context collection")?;

    if response.status().is_success() {
        return Ok(());
    }

    let create_url = format!("{base_url}/collections/{collection}");
    let create_body = json!({
        "vectors": {
            "size": 384,
            "distance": "Cosine"
        }
    });

    let create_response = client
        .put(&create_url)
        .json(&create_body)
        .send()
        .await
        .context("failed to create project context collection")?;

    if !create_response.status().is_success() {
        let payload = create_response
            .text()
            .await
            .unwrap_or_else(|_| "unknown".to_string());
        return Err(anyhow!(
            "unable to create project context collection: {payload}"
        ));
    }

    Ok(())
}

pub async fn upsert_prompt_correction_point(
    db: &PgPool,
    point_id: &str,
    vector: &[f32],
    payload: Value,
) -> anyhow::Result<()> {
    ensure_collection(db).await?;
    let (base_url, collection) = qdrant_config(db).await?;
    let url = format!("{base_url}/collections/{collection}/points?wait=true");

    let body = json!({
        "points": [
            {
                "id": point_id,
                "vector": vector,
                "payload": payload
            }
        ]
    });

    let response = nexus_http::build_client()
        .put(&url)
        .json(&body)
        .send()
        .await
        .context("failed to upsert qdrant point")?;

    if !response.status().is_success() {
        let payload = response
            .text()
            .await
            .unwrap_or_else(|_| "unknown".to_string());
        return Err(anyhow!("qdrant upsert failed: {payload}"));
    }

    Ok(())
}

pub async fn search_prompt_correction_points(
    db: &PgPool,
    query_vector: &[f32],
    project_id: Uuid,
    top_k: u64,
) -> anyhow::Result<Vec<VectorPointHit>> {
    ensure_collection(db).await?;
    let (base_url, collection) = qdrant_config(db).await?;
    let url = format!("{base_url}/collections/{collection}/points/search");

    let body = json!({
        "vector": query_vector,
        "limit": top_k.max(1),
        "with_payload": true,
        "with_vector": false,
        "filter": {
            "must": [
                {
                    "key": "project_id",
                    "match": { "value": project_id.to_string() }
                },
                {
                    "key": "active",
                    "match": { "value": true }
                }
            ]
        }
    });

    let response = nexus_http::build_client()
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("failed qdrant search")?;

    if !response.status().is_success() {
        let payload = response
            .text()
            .await
            .unwrap_or_else(|_| "unknown".to_string());
        return Err(anyhow!("qdrant search failed: {payload}"));
    }

    let payload: Value = response
        .json()
        .await
        .context("invalid qdrant search payload")?;
    let result = payload
        .get("result")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut hits = Vec::with_capacity(result.len());
    for hit in result {
        let score = hit.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let point_id = match hit.get("id") {
            Some(Value::String(value)) => value.clone(),
            Some(Value::Number(value)) => value.to_string(),
            _ => continue,
        };
        let point_payload = hit.get("payload").cloned().unwrap_or_else(|| json!({}));
        hits.push(VectorPointHit {
            point_id,
            score,
            payload: point_payload,
        });
    }

    Ok(hits)
}

pub async fn delete_prompt_correction_points(
    db: &PgPool,
    point_ids: &[String],
) -> anyhow::Result<usize> {
    if point_ids.is_empty() {
        return Ok(0);
    }

    ensure_collection(db).await?;
    let (base_url, collection) = qdrant_config(db).await?;
    let url = format!("{base_url}/collections/{collection}/points/delete?wait=true");
    let body = json!({
        "points": point_ids
    });

    let response = nexus_http::build_client()
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("failed qdrant delete")?;

    if !response.status().is_success() {
        let payload = response
            .text()
            .await
            .unwrap_or_else(|_| "unknown".to_string());
        return Err(anyhow!("qdrant delete failed: {payload}"));
    }

    Ok(point_ids.len())
}

/// Aggiorna il campo `active` nel payload di un punto Qdrant.
pub async fn set_point_active(db: &PgPool, point_id: &str, active: bool) -> Result<(), String> {
    let (base_url, collection) = qdrant_config(db).await.map_err(|e| e.to_string())?;
    let client = nexus_http::build_client();

    let body = json!({
        "points": [point_id],
        "payload": { "active": active }
    });

    let url = format!("{base_url}/collections/{collection}/points/payload");
    client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

pub async fn count_prompt_correction_points(
    db: &PgPool,
    project_id: Option<Uuid>,
) -> anyhow::Result<i64> {
    ensure_collection(db).await?;
    let (base_url, collection) = qdrant_config(db).await?;
    let url = format!("{base_url}/collections/{collection}/points/count");

    let filter = project_id.map(|project_id| {
        json!({
            "must": [
                {
                    "key": "project_id",
                    "match": { "value": project_id.to_string() }
                }
            ]
        })
    });

    let mut body = json!({
        "exact": true
    });
    if let Some(filter) = filter {
        body["filter"] = filter;
    }

    let response = nexus_http::build_client()
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("failed qdrant count")?;

    if !response.status().is_success() {
        let payload = response
            .text()
            .await
            .unwrap_or_else(|_| "unknown".to_string());
        return Err(anyhow!("qdrant count failed: {payload}"));
    }

    let payload: Value = response
        .json()
        .await
        .context("invalid qdrant count payload")?;
    let count = payload
        .get("result")
        .and_then(|value| value.get("count"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    Ok(count)
}

pub async fn upsert_project_context_point(
    db: &PgPool,
    point_id: &str,
    vector: &[f32],
    payload: Value,
) -> anyhow::Result<()> {
    ensure_project_context_collection(db).await?;
    let (base_url, collection) = qdrant_project_context_config(db).await?;
    let url = format!("{base_url}/collections/{collection}/points?wait=true");

    let body = json!({
        "points": [
            {
                "id": point_id,
                "vector": vector,
                "payload": payload
            }
        ]
    });

    let response = nexus_http::build_client()
        .put(&url)
        .json(&body)
        .send()
        .await
        .context("failed to upsert project context point")?;

    if !response.status().is_success() {
        let payload = response
            .text()
            .await
            .unwrap_or_else(|_| "unknown".to_string());
        return Err(anyhow!("project context upsert failed: {payload}"));
    }

    Ok(())
}

/// Ricerca semantica nella collezione `project_context` di Qdrant.
/// Usata da `nexus_builtin::docs` (doc generation) e
/// `agent_tools::semantic_tools` per il recupero della knowledge base.
pub async fn search_project_context_points(
    db: &PgPool,
    query_vector: &[f32],
    project_id: Uuid,
    top_k: u64,
    min_score: f64,
) -> anyhow::Result<Vec<VectorPointHit>> {
    ensure_project_context_collection(db).await?;
    let (base_url, collection) = qdrant_project_context_config(db).await?;
    let url = format!("{base_url}/collections/{collection}/points/search");

    let body = json!({
        "vector": query_vector,
        "limit": top_k.max(1),
        "with_payload": true,
        "with_vector": false,
        "score_threshold": min_score,
        "filter": {
            "must": [
                {
                    "key": "project_id",
                    "match": { "value": project_id.to_string() }
                }
            ]
        }
    });

    let response = nexus_http::build_client()
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("failed qdrant project context search")?;

    if !response.status().is_success() {
        let text = response
            .text()
            .await
            .unwrap_or_else(|_| "unknown".to_string());
        return Err(anyhow!("qdrant project context search failed: {text}"));
    }

    let payload: Value = response
        .json()
        .await
        .context("invalid qdrant project context payload")?;
    let result = payload
        .get("result")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut hits = Vec::with_capacity(result.len());
    for hit in result {
        let score = hit.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let point_id = match hit.get("id") {
            Some(Value::String(value)) => value.clone(),
            Some(Value::Number(value)) => value.to_string(),
            _ => continue,
        };
        let point_payload = hit.get("payload").cloned().unwrap_or_else(|| json!({}));
        hits.push(VectorPointHit {
            point_id,
            score,
            payload: point_payload,
        });
    }

    Ok(hits)
}

pub async fn delete_project_bootstrap_points(db: &PgPool, project_id: Uuid) -> anyhow::Result<()> {
    ensure_project_context_collection(db).await?;
    let (base_url, collection) = qdrant_project_context_config(db).await?;
    let url = format!("{base_url}/collections/{collection}/points/delete?wait=true");
    let body = json!({
        "filter": {
            "must": [
                {
                    "key": "project_id",
                    "match": { "value": project_id.to_string() }
                },
                {
                    "key": "type",
                    "match": { "value": "project_bootstrap" }
                }
            ]
        }
    });

    let response = nexus_http::build_client()
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("failed to delete project bootstrap points")?;

    if !response.status().is_success() {
        let payload = response
            .text()
            .await
            .unwrap_or_else(|_| "unknown".to_string());
        return Err(anyhow!("project bootstrap delete failed: {payload}"));
    }

    Ok(())
}

pub async fn project_context_collection_name(db: &PgPool) -> anyhow::Result<String> {
    let (_, collection) = qdrant_project_context_config(db).await?;
    Ok(collection)
}

// ── Code Index collection ────────────────────────────────────────────────────

async fn qdrant_code_index_config(db: &PgPool) -> anyhow::Result<(String, String)> {
    let url = get_setting(db, "qdrant_url").await?.unwrap_or_else(|| {
        std::env::var("QDRANT_URL").unwrap_or_else(|_| DEFAULT_QDRANT_URL.to_string())
    });

    let collection = get_setting(db, "qdrant_code_index_collection")
        .await?
        .or_else(|| std::env::var("QDRANT_CODE_INDEX_COLLECTION").ok())
        .unwrap_or_else(|| DEFAULT_CODE_INDEX_COLLECTION.to_string());

    Ok((url, collection))
}

async fn ensure_code_index_collection(db: &PgPool) -> anyhow::Result<()> {
    let (base_url, collection) = qdrant_code_index_config(db).await?;
    let client = nexus_http::build_client();
    let get_url = format!("{base_url}/collections/{collection}");

    let response = client
        .get(&get_url)
        .send()
        .await
        .context("failed to check code index collection")?;

    if response.status().is_success() {
        return Ok(());
    }

    let create_url = format!("{base_url}/collections/{collection}");
    let create_body = json!({
        "vectors": {
            "size": 384,
            "distance": "Cosine"
        }
    });

    let create_response = client
        .put(&create_url)
        .json(&create_body)
        .send()
        .await
        .context("failed to create code index collection")?;

    if !create_response.status().is_success() {
        let payload = create_response
            .text()
            .await
            .unwrap_or_else(|_| "unknown".to_string());
        return Err(anyhow!("unable to create code index collection: {payload}"));
    }

    Ok(())
}

pub async fn upsert_code_index_point(
    db: &PgPool,
    point_id: &str,
    vector: &[f32],
    payload: Value,
) -> anyhow::Result<()> {
    ensure_code_index_collection(db).await?;
    let (base_url, collection) = qdrant_code_index_config(db).await?;
    let url = format!("{base_url}/collections/{collection}/points?wait=true");

    let body = json!({
        "points": [
            {
                "id": point_id,
                "vector": vector,
                "payload": payload
            }
        ]
    });

    let response = nexus_http::build_client()
        .put(&url)
        .json(&body)
        .send()
        .await
        .context("failed to upsert code index point")?;

    if !response.status().is_success() {
        let payload = response
            .text()
            .await
            .unwrap_or_else(|_| "unknown".to_string());
        return Err(anyhow!("code index upsert failed: {payload}"));
    }

    Ok(())
}

pub async fn search_code_index(
    db: &PgPool,
    vector: &[f32],
    project_id: Uuid,
    limit: usize,
) -> anyhow::Result<Vec<VectorPointHit>> {
    ensure_code_index_collection(db).await?;
    let (base_url, collection) = qdrant_code_index_config(db).await?;
    let url = format!("{base_url}/collections/{collection}/points/search");

    let body = json!({
        "vector": vector,
        "limit": limit.max(1),
        "with_payload": true,
        "with_vector": false,
        "filter": {
            "must": [
                {
                    "key": "project_id",
                    "match": { "value": project_id.to_string() }
                },
                {
                    "key": "active",
                    "match": { "value": true }
                }
            ]
        }
    });

    let response = nexus_http::build_client()
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("failed qdrant code index search")?;

    if !response.status().is_success() {
        let payload = response
            .text()
            .await
            .unwrap_or_else(|_| "unknown".to_string());
        return Err(anyhow!("qdrant code index search failed: {payload}"));
    }

    let payload: Value = response
        .json()
        .await
        .context("invalid qdrant code index search payload")?;
    let result = payload
        .get("result")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut hits = Vec::with_capacity(result.len());
    for hit in result {
        let score = hit.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let point_id = match hit.get("id") {
            Some(Value::String(value)) => value.clone(),
            Some(Value::Number(value)) => value.to_string(),
            _ => continue,
        };
        let point_payload = hit.get("payload").cloned().unwrap_or_else(|| json!({}));
        hits.push(VectorPointHit {
            point_id,
            score,
            payload: point_payload,
        });
    }

    Ok(hits)
}

/// Ritorna `true` se il progetto ha almeno un file indicizzato in `file_index_hashes`.
/// Query O(1) sull'indice — usata da `spawn_code_index_if_needed` per evitare
/// di rilanciare l'indicizzazione su progetti gia' processati.
pub async fn has_code_index(db: &PgPool, project_id: Uuid) -> bool {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT COUNT(*) FROM file_index_hashes WHERE project_id = $1 LIMIT 1")
            .bind(project_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    row.map(|(c,)| c > 0).unwrap_or(false)
}

pub async fn delete_code_index_points(db: &PgPool, project_id: Uuid) -> anyhow::Result<()> {
    ensure_code_index_collection(db).await?;
    let (base_url, collection) = qdrant_code_index_config(db).await?;
    let url = format!("{base_url}/collections/{collection}/points/delete?wait=true");
    let body = json!({
        "filter": {
            "must": [
                {
                    "key": "project_id",
                    "match": { "value": project_id.to_string() }
                }
            ]
        }
    });

    let response = nexus_http::build_client()
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("failed to delete code index points")?;

    if !response.status().is_success() {
        let payload = response
            .text()
            .await
            .unwrap_or_else(|_| "unknown".to_string());
        return Err(anyhow!("code index delete failed: {payload}"));
    }

    Ok(())
}

/// Cancella i chunk di un singolo file dal code index (usato prima di re-indicizzare).
pub async fn delete_code_index_file_points(
    db: &PgPool,
    project_id: Uuid,
    file_path: &str,
) -> anyhow::Result<()> {
    ensure_code_index_collection(db).await?;
    let (base_url, collection) = qdrant_code_index_config(db).await?;
    let url = format!("{base_url}/collections/{collection}/points/delete?wait=true");
    let body = json!({
        "filter": {
            "must": [
                { "key": "project_id", "match": { "value": project_id.to_string() } },
                { "key": "file_path",  "match": { "value": file_path } }
            ]
        }
    });

    let response = nexus_http::build_client()
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("failed to delete code index file points")?;

    if !response.status().is_success() {
        let text = response
            .text()
            .await
            .unwrap_or_else(|_| "unknown".to_string());
        return Err(anyhow!("code index file delete failed: {text}"));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// project_docs collection — documenti di progetto vettorializzati
// ---------------------------------------------------------------------------

const DEFAULT_DOCS_COLLECTION: &str = "project_docs";

async fn qdrant_docs_config(db: &PgPool) -> anyhow::Result<(String, String)> {
    let url = crate::settings::get_setting(db, "qdrant_url")
        .await?
        .unwrap_or_else(|| {
            std::env::var("QDRANT_URL").unwrap_or_else(|_| DEFAULT_QDRANT_URL.to_string())
        });
    Ok((url, DEFAULT_DOCS_COLLECTION.to_string()))
}

async fn ensure_docs_collection(db: &PgPool) -> anyhow::Result<()> {
    let (base_url, collection) = qdrant_docs_config(db).await?;
    let check_url = format!("{base_url}/collections/{collection}");
    let resp = reqwest::Client::new().get(&check_url).send().await;
    if let Ok(r) = resp {
        if r.status().is_success() {
            return Ok(());
        }
    }
    let create_url = format!("{base_url}/collections/{collection}");
    let body = json!({
        "vectors": { "size": 384, "distance": "Cosine" }
    });
    let resp = reqwest::Client::new()
        .put(&create_url)
        .json(&body)
        .send()
        .await
        .context("failed to create project_docs collection")?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("failed to create project_docs collection: {text}"));
    }
    tracing::info!("Created Qdrant collection: {collection}");
    Ok(())
}

/// Vectorize a document: chunk by sections, embed, upsert to Qdrant.
pub async fn vectorize_document(
    db: &PgPool,
    project_id: Uuid,
    document_id: Uuid,
    doc_type: &str,
    version: &str,
    content: &Value,
) -> anyhow::Result<()> {
    // Guard: se embedder/qdrant sono down a livello globale, skip immediato.
    // Usiamo un approccio leggero: probe HTTP locale a Qdrant con timeout 2s.
    // Se non risponde, evitiamo l'intera operazione.
    // NOTA: questo guard e' ridondante se il caller ha gia' consultato DependencyStatus,
    // ma serve come difesa per i tokio::spawn fire-and-forget che non hanno accesso allo stato.
    ensure_docs_collection(db).await?;

    let neural_url = crate::settings::get_setting(db, "neural_core_url")
        .await?
        .unwrap_or_else(|| {
            std::env::var("NEURAL_CORE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:50051".to_string())
        });
    let neural = crate::orchestrator::NeuralCoreClient::connect(&neural_url).await?;

    // Delete old points for this document
    delete_doc_points(db, project_id, document_id).await.ok();

    let sections = content.get("sections").and_then(Value::as_array);
    let sections = match sections {
        Some(s) => s,
        None => {
            tracing::warn!("No sections to vectorize for document {document_id}");
            return Ok(());
        }
    };

    let mut point_ids = Vec::new();
    let (base_url, collection) = qdrant_docs_config(db).await?;
    let url = format!("{base_url}/collections/{collection}/points?wait=true");

    let flat = flatten_sections(sections);
    for (number, title, content) in &flat {
        let text_to_embed = format!("{number} {title}\n{content}");
        let text_preview = if content.len() > 200 {
            &content[..200]
        } else {
            content.as_str()
        };

        let vector = match neural.embed_text("", &text_to_embed).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Embed error for section {number}: {e}");
                continue;
            }
        };

        let point_id = format!(
            "{:x}",
            Sha256::digest(
                format!("{}:doc:{}:{}:{}", project_id, document_id, number, version).as_bytes()
            )
        );

        let payload = json!({
            "project_id": project_id.to_string(),
            "document_id": document_id.to_string(),
            "doc_type": doc_type,
            "section_path": number,
            "section_title": title,
            "version": version,
            "text_preview": text_preview,
            "active": true,
        });

        let body = json!({ "points": [{ "id": point_id, "vector": vector, "payload": payload }] });

        match reqwest::Client::new().put(&url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {
                point_ids.push(point_id);
            }
            Ok(resp) => {
                let t = resp.text().await.unwrap_or_default();
                tracing::warn!("Doc point upsert failed: {t}");
            }
            Err(e) => {
                tracing::warn!("Doc point upsert error: {e}");
            }
        }
    }

    // Save point IDs to DB
    if !point_ids.is_empty() {
        let _ = sqlx::query("UPDATE project_documents SET qdrant_point_ids = $1 WHERE id = $2")
            .bind(&point_ids)
            .bind(document_id)
            .execute(db)
            .await;
    }

    tracing::info!(
        "Vectorized document {document_id}: {} points",
        point_ids.len()
    );
    Ok(())
}

/// Flatten sections recursively into a list of (number, title, content) tuples.
fn flatten_sections(sections: &[Value]) -> Vec<(String, String, String)> {
    let mut result = Vec::new();
    let mut stack: Vec<&Value> = sections.iter().rev().collect();
    while let Some(section) = stack.pop() {
        let number = section
            .get("number")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let title = section
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let content = section
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !content.is_empty() {
            result.push((number, title, content));
        }
        if let Some(subs) = section.get("subsections").and_then(Value::as_array) {
            for sub in subs.iter().rev() {
                stack.push(sub);
            }
        }
    }
    result
}

pub async fn search_doc_points(
    db: &PgPool,
    query_vector: &[f32],
    project_id: Uuid,
    doc_type: Option<&str>,
    limit: usize,
) -> anyhow::Result<Vec<VectorPointHit>> {
    ensure_docs_collection(db).await?;
    let (base_url, collection) = qdrant_docs_config(db).await?;
    let url = format!("{base_url}/collections/{collection}/points/search");

    let mut must_filters = vec![
        json!({ "key": "project_id", "match": { "value": project_id.to_string() } }),
        json!({ "key": "active", "match": { "value": true } }),
    ];

    if let Some(dt) = doc_type {
        must_filters.push(json!({ "key": "doc_type", "match": { "value": dt } }));
    }

    let body = json!({
        "vector": query_vector,
        "limit": limit.max(1),
        "with_payload": true,
        "with_vector": false,
        "filter": { "must": must_filters }
    });

    let response = reqwest::Client::new().post(&url).json(&body).send().await?;
    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(anyhow!("qdrant doc search failed: {text}"));
    }

    let payload: Value = response.json().await?;
    let results = payload
        .get("result")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    Ok(results
        .iter()
        .map(|r| VectorPointHit {
            point_id: r
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            score: r.get("score").and_then(Value::as_f64).unwrap_or(0.0),
            payload: r.get("payload").cloned().unwrap_or(json!({})),
        })
        .collect())
}

pub async fn delete_doc_points(
    db: &PgPool,
    project_id: Uuid,
    document_id: Uuid,
) -> anyhow::Result<()> {
    ensure_docs_collection(db).await?;
    let (base_url, collection) = qdrant_docs_config(db).await?;
    let url = format!("{base_url}/collections/{collection}/points/delete?wait=true");
    let body = json!({
        "filter": {
            "must": [
                { "key": "project_id", "match": { "value": project_id.to_string() } },
                { "key": "document_id", "match": { "value": document_id.to_string() } }
            ]
        }
    });

    let response = reqwest::Client::new().post(&url).json(&body).send().await?;
    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(anyhow!("qdrant doc delete failed: {text}"));
    }
    Ok(())
}

pub async fn delete_doc_points_by_ids(db: &PgPool, point_ids: &[String]) -> anyhow::Result<()> {
    if point_ids.is_empty() {
        return Ok(());
    }
    ensure_docs_collection(db).await?;
    let (base_url, collection) = qdrant_docs_config(db).await?;
    let url = format!("{base_url}/collections/{collection}/points/delete?wait=true");
    let body = json!({ "points": point_ids });
    let response = reqwest::Client::new().post(&url).json(&body).send().await?;
    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(anyhow!("qdrant doc delete by ids failed: {text}"));
    }
    Ok(())
}

// ── Conversation Context collection (contesto vettoriale) ───────────────────

const DEFAULT_CONVERSATION_CONTEXT_COLLECTION: &str = "conversation_context";

async fn qdrant_conversation_context_config(db: &PgPool) -> anyhow::Result<(String, String)> {
    let url = get_setting(db, "qdrant_url").await?.unwrap_or_else(|| {
        std::env::var("QDRANT_URL").unwrap_or_else(|_| DEFAULT_QDRANT_URL.to_string())
    });
    let collection = get_setting(db, "qdrant_conversation_context_collection")
        .await?
        .unwrap_or_else(|| DEFAULT_CONVERSATION_CONTEXT_COLLECTION.to_string());
    Ok((url, collection))
}

async fn ensure_conversation_context_collection(db: &PgPool) -> anyhow::Result<()> {
    let (base_url, collection) = qdrant_conversation_context_config(db).await?;
    let client = nexus_http::build_client();
    let get_url = format!("{base_url}/collections/{collection}");

    let response = client
        .get(&get_url)
        .send()
        .await
        .context("check conversation_context collection")?;
    if response.status().is_success() {
        return Ok(());
    }

    let create_body = json!({
        "vectors": { "size": 384, "distance": "Cosine" }
    });
    let create_response = client
        .put(&get_url)
        .json(&create_body)
        .send()
        .await
        .context("create conversation_context collection")?;
    if !create_response.status().is_success() {
        let text = create_response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "create conversation_context collection failed: {text}"
        ));
    }
    Ok(())
}

/// Salva un turno conversazionale (user o assistant) nella collection Qdrant.
/// `point_id` dovrebbe essere deterministico: sha256(session_id + message_id).
pub async fn upsert_conversation_turn(
    db: &PgPool,
    point_id: &str,
    vector: &[f32],
    session_id: Uuid,
    role: &str,
    content: &str,
    created_at: &str,
) -> anyhow::Result<()> {
    ensure_conversation_context_collection(db).await?;
    let (base_url, collection) = qdrant_conversation_context_config(db).await?;
    let url = format!("{base_url}/collections/{collection}/points?wait=true");

    let body = json!({
        "points": [{
            "id": point_id,
            "vector": vector,
            "payload": {
                "session_id": session_id.to_string(),
                "role": role,
                "content": content,
                "created_at": created_at,
            }
        }]
    });

    let response = nexus_http::build_client()
        .put(&url)
        .json(&body)
        .send()
        .await
        .context("upsert conversation turn")?;
    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(anyhow!("conversation turn upsert failed: {text}"));
    }
    Ok(())
}

/// Cerca i turni conversazionali semanticamente piu' simili nella stessa sessione.
pub async fn search_conversation_context(
    db: &PgPool,
    query_vector: &[f32],
    session_id: Uuid,
    top_k: u64,
    min_score: f64,
) -> anyhow::Result<Vec<VectorPointHit>> {
    ensure_conversation_context_collection(db).await?;
    let (base_url, collection) = qdrant_conversation_context_config(db).await?;
    let url = format!("{base_url}/collections/{collection}/points/search");

    let body = json!({
        "vector": query_vector,
        "limit": top_k.max(1),
        "with_payload": true,
        "with_vector": false,
        "score_threshold": min_score,
        "filter": {
            "must": [{
                "key": "session_id",
                "match": { "value": session_id.to_string() }
            }]
        }
    });

    let response = nexus_http::build_client()
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("conversation context search")?;
    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(anyhow!("conversation context search failed: {text}"));
    }

    let payload: Value = response
        .json()
        .await
        .context("invalid conversation context payload")?;
    let result = payload
        .get("result")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut hits = Vec::with_capacity(result.len());
    for hit in result {
        let score = hit.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let point_id = match hit.get("id") {
            Some(Value::String(v)) => v.clone(),
            Some(Value::Number(v)) => v.to_string(),
            _ => continue,
        };
        let pl = hit.get("payload").cloned().unwrap_or(json!({}));
        hits.push(VectorPointHit {
            point_id,
            score,
            payload: pl,
        });
    }
    Ok(hits)
}

/// Genera un point_id deterministico per un turno conversazionale.
pub fn conversation_point_id(session_id: Uuid, message_id: Uuid) -> String {
    let mut hasher = Sha256::new();
    hasher.update(session_id.as_bytes());
    hasher.update(message_id.as_bytes());
    let hash = hasher.finalize();
    format!("{:x}", hash)[..32].to_string()
}

// ═══════════════════════════════════════════════════════════════════════════
// Wiki content (ADR 0017 v2) — collection Qdrant unificata per `wiki_docs`.
//
// Singola collection, niente partizionamento per scope: la discriminazione
// tra meta/project vive nel payload (`scope` + `project_id`). Vector size e
// distance identiche a `nexus_meta_docs` (384 / Cosine).
// ═══════════════════════════════════════════════════════════════════════════

const DEFAULT_WIKI_CONTENT_COLLECTION: &str = "wiki_content";

async fn qdrant_wiki_content_config(db: &PgPool) -> anyhow::Result<(String, String)> {
    let url = get_setting(db, "qdrant_url").await?.unwrap_or_else(|| {
        std::env::var("QDRANT_URL").unwrap_or_else(|_| DEFAULT_QDRANT_URL.to_string())
    });
    let collection = get_setting(db, "agent.wiki.qdrant_collection")
        .await?
        .unwrap_or_else(|| DEFAULT_WIKI_CONTENT_COLLECTION.to_string());
    Ok((url, collection))
}

/// Crea la collection Qdrant `wiki_content` se non esiste (idempotente).
pub async fn ensure_wiki_content_collection(db: &PgPool) -> anyhow::Result<()> {
    let (base_url, collection) = qdrant_wiki_content_config(db).await?;
    let client = nexus_http::build_client();
    let get_url = format!("{base_url}/collections/{collection}");

    let response = client
        .get(&get_url)
        .send()
        .await
        .context("impossibile verificare collection wiki_content")?;

    if response.status().is_success() {
        return Ok(());
    }

    let create_url = format!("{base_url}/collections/{collection}");
    let create_body = json!({
        "vectors": {
            "size": 384,
            "distance": "Cosine"
        }
    });

    let create_response = client
        .put(&create_url)
        .json(&create_body)
        .send()
        .await
        .context("impossibile creare collection wiki_content")?;

    if !create_response.status().is_success() {
        let payload = create_response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "creazione collection wiki_content fallita: {payload}"
        ));
    }

    Ok(())
}

/// Inserisce/aggiorna un punto nella collection `wiki_content`. Il payload
/// e' libero (vedi `wiki::reingest::build_payload`) ma ci si aspetta almeno
/// `scope`, `doc_id`, `title`. La funzione non assume scope-specifico.
pub async fn upsert_wiki_content_point(
    db: &PgPool,
    point_id: &str,
    vector: Vec<f32>,
    payload: Value,
) -> anyhow::Result<()> {
    ensure_wiki_content_collection(db).await?;
    let (base_url, collection) = qdrant_wiki_content_config(db).await?;
    let url = format!("{base_url}/collections/{collection}/points?wait=true");
    let body = json!({
        "points": [{
            "id": point_id,
            "vector": vector,
            "payload": payload,
        }]
    });

    let client = nexus_http::build_client();
    let response = client
        .put(&url)
        .json(&body)
        .send()
        .await
        .context("wiki_content upsert fallito")?;

    if !response.status().is_success() {
        let payload = response.text().await.unwrap_or_default();
        return Err(anyhow!("wiki_content upsert fallito: {payload}"));
    }
    Ok(())
}

/// Recupera il vector di un point in `wiki_content` (id = doc UUID stringa).
/// Permette il link semantico senza ri-embeddare il body. Errore se assente.
pub async fn get_wiki_content_point_vector(
    db: &PgPool,
    point_id: &str,
) -> anyhow::Result<Vec<f32>> {
    let (base_url, collection) = qdrant_wiki_content_config(db).await?;
    let url = format!("{base_url}/collections/{collection}/points/{point_id}?with_vector=true");
    let client = nexus_http::build_client();
    let response = client
        .get(&url)
        .send()
        .await
        .context("get_wiki_content_point_vector: GET point fallito")?;
    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(anyhow!("get_wiki_content_point_vector: HTTP error: {text}"));
    }
    let payload: Value = response
        .json()
        .await
        .context("get_wiki_content_point_vector: parse JSON")?;
    let vector = payload
        .get("result")
        .and_then(|r| r.get("vector"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("get_wiki_content_point_vector: vector mancante"))?
        .iter()
        .filter_map(|x| x.as_f64().map(|f| f as f32))
        .collect::<Vec<f32>>();
    if vector.is_empty() {
        return Err(anyhow!("get_wiki_content_point_vector: vettore vuoto"));
    }
    Ok(vector)
}

/// Cerca in `wiki_content` per cosine similarity. Filtri ACL applicati a valle
/// dal chiamante (il payload Qdrant contiene `scope` + `project_id`).
/// `score_threshold` viene applicato lato Qdrant. Ritorna hit ordinati per score.
pub async fn search_wiki_content_points(
    db: &PgPool,
    vector: Vec<f32>,
    top_k: usize,
    score_threshold: f64,
) -> anyhow::Result<Vec<VectorPointHit>> {
    search_wiki_content_points_filtered(db, vector, top_k, score_threshold, None).await
}

/// Variante di `search_wiki_content_points` con filtro Qdrant arbitrario sul
/// payload. Il `filter` (se `Some`) viene inoltrato nel body della query come
/// `{ "filter": ... }` -> rispetta la sintassi Qdrant
/// (es. `{ "must": [{ "key": "scope", "match": { "value": "meta" } }, ...] }`).
///
/// Usato da `wiki::search` per restringere lato server lo scope/progetto/kind/tags
/// prima ancora del filtro ACL applicato in Postgres a valle.
pub async fn search_wiki_content_points_filtered(
    db: &PgPool,
    vector: Vec<f32>,
    top_k: usize,
    score_threshold: f64,
    filter: Option<Value>,
) -> anyhow::Result<Vec<VectorPointHit>> {
    ensure_wiki_content_collection(db).await?;
    let (base_url, collection) = qdrant_wiki_content_config(db).await?;
    let url = format!("{base_url}/collections/{collection}/points/search");
    let mut body = json!({
        "vector": vector,
        "limit": top_k,
        "score_threshold": score_threshold,
        "with_payload": true,
        "with_vector": false,
    });
    if let Some(f) = filter {
        if let Some(obj) = body.as_object_mut() {
            obj.insert("filter".to_string(), f);
        }
    }

    let client = nexus_http::build_client();
    let response = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("wiki_content search fallita")?;
    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(anyhow!("wiki_content search fallita: {text}"));
    }
    let payload: Value = response
        .json()
        .await
        .context("payload ricerca wiki_content non valido")?;
    let result = payload
        .get("result")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut hits = Vec::with_capacity(result.len());
    for hit in result {
        let score = hit.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let point_id = match hit.get("id") {
            Some(Value::String(v)) => v.clone(),
            Some(Value::Number(v)) => v.to_string(),
            _ => continue,
        };
        let pl = hit.get("payload").cloned().unwrap_or(json!({}));
        hits.push(VectorPointHit {
            point_id,
            score,
            payload: pl,
        });
    }
    Ok(hits)
}
