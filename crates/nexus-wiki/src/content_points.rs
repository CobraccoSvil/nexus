//! Famiglia Qdrant wiki content points (collection `wiki_content`, ADR 0017 v2).
//!
//! Estratta da mcp-core::vector_memory (split 7.4): le funzioni prendono solo
//! `&PgPool` e leggono la config dai settings (regola G). mcp-core re-esporta
//! questi simboli per i call site fuori dal wiki (chat_attachments,
//! deep_analyze, agent_tools::knowledge, chat_messages::context).

use anyhow::{anyhow, Context};
use serde_json::{json, Value};
use sqlx::PgPool;

use nexus_auth::get_setting_nonempty as get_setting;
use nexus_types::vector_dto::VectorPointHit;

const DEFAULT_QDRANT_URL: &str = "http://localhost:6333";
// ═══════════════════════════════════════════════════════════════════════════
// Wiki content (ADR 0017 v2) — collection Qdrant unificata per `wiki_docs`.
//
// Singola collection, niente partizionamento per scope: la discriminazione
// tra meta/project vive nel payload (`scope` + `project_id`). Vector size e
// distance identiche a `nexus_meta_docs` (384 / Cosine).
// ═══════════════════════════════════════════════════════════════════════════

const DEFAULT_WIKI_CONTENT_COLLECTION: &str = "wiki_content";

/// La chiave di payload che distingue meta e progetto DENTRO `wiki_content`.
///
/// La collection e' UNA sola per entrambi gli scope (nessun partizionamento:
/// la discriminazione vive nel payload), quindi chi la interroga senza questo
/// filtro legge anche i documenti degli ALTRI progetti. Il valore ammesso lo
/// dichiara [`crate::model::WikiScope::as_str`], mai un letterale.
pub const CHIAVE_SCOPE: &str = "scope";

/// Punto unico (regola L) del NOME della collection del wiki.
///
/// Lo SCRITTORE e' questo modulo; i LETTORI sono due e stanno fuori — la
/// famiglia `knowledge.*` (che passa dalle funzioni qui sotto) e il RAG di
/// mcp-core, che per [`SourceKind::Kb`](nexus_types::source_kind::SourceKind)
/// e `MetaDoc` interroga QUESTA collection e non una propria.
///
/// La funzione esiste perche' il secondo lettore aveva inciso due nomi suoi.
/// Vedi `mcp-core::rag::collezioni` per la misura.
pub async fn wiki_content_collection(db: &PgPool) -> anyhow::Result<String> {
    let (_, collection) = qdrant_wiki_content_config(db).await?;
    Ok(collection)
}

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
