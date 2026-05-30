//! MCP runtime discovery + call tools.
//!
//! Obiettivo: ridurre token verso il provider evitando di inviare tutte le tool definitions MCP
//! in ogni richiesta. L'agente può:
//! 1) cercare tool con `nexus_mcp_tool_search` (semantico Qdrant + ILIKE fallback)
//! 2) invocare tool specifico con `nexus_mcp_tool_call` (server_id + tool_name + arguments)
//! 3) ricostruire l'indice semantico con `nexus_mcp_tool_reindex` (admin)
//!
//! Sicurezza:
//! - la search e la call sono limitate ai server MCP accessibili (scope global/user/project)
//! - la call applica anche la policy del plugin (via mcp_connectors::execute_mcp_tool)
//!
//! Indicizzazione semantica:
//! - Ogni tool viene vettorializzato su "tool_name: description" (384D cl100k-like)
//! - La collection Qdrant usata è configurabile via setting `qdrant_mcp_tools_collection`
//! - L'hash SHA-256 dei primi 256 char di (name+description) serve per skip idempotente

use sha2::{Digest, Sha256};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::mcp_connectors;
use crate::orchestrator::NeuralCoreClient;

// ── Costanti ─────────────────────────────────────────────────────────────────

const DEFAULT_QDRANT_URL: &str = "http://localhost:6333";
const DEFAULT_COLLECTION: &str = "mcp_tools";
const VECTOR_SIZE: u64 = 384;
const DEFAULT_MIN_SCORE: f64 = 0.35;

// ── Helpers DB settings ───────────────────────────────────────────────────────

async fn get_setting(db: &PgPool, key: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

async fn qdrant_url(db: &PgPool) -> String {
    get_setting(db, "qdrant_url").await
        .or_else(|| std::env::var("QDRANT_URL").ok())
        .unwrap_or_else(|| DEFAULT_QDRANT_URL.to_string())
}

async fn collection_name(db: &PgPool) -> String {
    get_setting(db, "qdrant_mcp_tools_collection").await
        .unwrap_or_else(|| DEFAULT_COLLECTION.to_string())
}

async fn min_score(db: &PgPool) -> f64 {
    get_setting(db, "mcp_tool_search_min_score").await
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(DEFAULT_MIN_SCORE)
        .clamp(0.0, 1.0)
}

fn parse_i64(v: Option<&Value>, default: i64) -> i64 {
    v.and_then(Value::as_i64).unwrap_or(default)
}

fn parse_str(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

// ── Sicurezza accesso server ──────────────────────────────────────────────────

async fn can_access_server(
    db: &PgPool,
    server_id: Uuid,
    user_id: Uuid,
    project_id: Uuid,
) -> bool {
    let row = sqlx::query(
        "SELECT scope, user_id, project_id, enabled FROM mcp_servers WHERE id=$1",
    )
    .bind(server_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(row) = row else { return false; };
    let enabled: bool = row.try_get("enabled").unwrap_or(false);
    if !enabled { return false; }

    let scope: String = row.try_get("scope").unwrap_or_else(|_| "user".to_string());
    let owner: Option<Uuid> = row.try_get("user_id").unwrap_or(None);
    let pid: Option<Uuid> = row.try_get("project_id").unwrap_or(None);

    match scope.as_str() {
        "global" => true,
        "project" => pid == Some(project_id),
        _ => owner == Some(user_id),
    }
}

// ── Qdrant collection ensure ──────────────────────────────────────────────────

async fn ensure_mcp_tools_collection(db: &PgPool) -> anyhow::Result<()> {
    let base = qdrant_url(db).await;
    let coll = collection_name(db).await;
    let client = nexus_http::build_client();
    let get_url = format!("{base}/collections/{coll}");

    let resp = client.get(&get_url).send().await?;
    if resp.status().is_success() {
        return Ok(());
    }

    let create_url = format!("{base}/collections/{coll}");
    let body = json!({
        "vectors": { "size": VECTOR_SIZE, "distance": "Cosine" }
    });
    let r = client.put(&create_url).json(&body).send().await?;
    if !r.status().is_success() {
        let msg = r.text().await.unwrap_or_else(|_| "?".into());
        anyhow::bail!("Qdrant create collection fallita: {msg}");
    }
    Ok(())
}

// ── Indicizzazione semantica ──────────────────────────────────────────────────

/// Calcola un point_id deterministico (UUID v5 nello spazio nil) per un tool.
fn tool_point_id(server_id: Uuid, tool_name: &str) -> String {
    // Usiamo SHA-256 dei primi 32 char del concat per avere un ID stabile.
    // Qdrant accetta UUID string oppure u64. Usiamo il truncated SHA come stringa.
    let mut h = Sha256::new();
    h.update(server_id.as_bytes());
    h.update(b"|");
    h.update(tool_name.as_bytes());
    let digest = h.finalize();
    // Prendi i primi 16 byte e formatta come UUID
    let bytes: [u8; 16] = digest[..16].try_into().unwrap();
    Uuid::from_bytes(bytes).to_string()
}

/// Hash dei primi 256 char di (name + description) per rilevare cambiamenti.
fn embedding_hash(tool_name: &str, description: &str) -> String {
    let mut h = Sha256::new();
    let combined = format!("{tool_name}:{description}");
    h.update(combined.as_bytes().iter().take(256).copied().collect::<Vec<_>>());
    format!("{:x}", h.finalize())
}

/// Genera il testo da vettorializzare per un tool.
fn embed_text_for_tool(tool_name: &str, description: &str) -> String {
    if description.is_empty() {
        tool_name.to_string()
    } else {
        format!("{tool_name}: {description}")
    }
}

/// Indicizza un singolo tool in Qdrant + aggiorna `embedding_hash`/`embedded_at` nel DB.
/// Idempotente: skip se hash invariato.
pub async fn index_tool(
    db: &PgPool,
    neural: &NeuralCoreClient,
    server_id: Uuid,
    server_name: &str,
    tool_name: &str,
    description: &str,
    scope: &str,
) -> anyhow::Result<()> {
    let new_hash = embedding_hash(tool_name, description);

    // Controlla se già indicizzato con stesso hash
    let existing: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT embedding_hash FROM mcp_server_tools WHERE server_id=$1 AND tool_name=$2",
    )
    .bind(server_id)
    .bind(tool_name)
    .fetch_optional(db)
    .await
    .unwrap_or(None);

    if let Some((Some(old_hash),)) = existing {
        if old_hash == new_hash {
            return Ok(()); // Nessun cambiamento, skip
        }
    }

    // Genera embedding
    let text = embed_text_for_tool(tool_name, description);
    let vector = neural.embed_text("", &text).await
        .map_err(|e| anyhow::anyhow!("embed_text fallita: {e}"))?;

    // Upsert in Qdrant
    ensure_mcp_tools_collection(db).await?;
    let base = qdrant_url(db).await;
    let coll = collection_name(db).await;
    let url = format!("{base}/collections/{coll}/points?wait=true");
    let point_id = tool_point_id(server_id, tool_name);

    let body = json!({
        "points": [{
            "id": point_id,
            "vector": vector,
            "payload": {
                "server_id": server_id.to_string(),
                "server_name": server_name,
                "tool_name": tool_name,
                "description": description,
                "scope": scope,
            }
        }]
    });

    let r = nexus_http::build_client().put(&url).json(&body).send().await?;
    if !r.status().is_success() {
        let msg = r.text().await.unwrap_or_else(|_| "?".into());
        anyhow::bail!("Qdrant upsert fallita: {msg}");
    }

    // Aggiorna hash + timestamp nel DB
    let _ = sqlx::query(
        "UPDATE mcp_server_tools SET embedding_hash=$1, embedded_at=NOW() WHERE server_id=$2 AND tool_name=$3",
    )
    .bind(&new_hash)
    .bind(server_id)
    .bind(tool_name)
    .execute(db)
    .await;

    Ok(())
}

// ── Ricerca semantica Qdrant ──────────────────────────────────────────────────

async fn semantic_search(
    db: &PgPool,
    neural: &NeuralCoreClient,
    query: &str,
    user_id: Uuid,
    project_id: Uuid,
    limit: i64,
) -> anyhow::Result<Vec<Value>> {
    let vector = neural.embed_text("", query).await?;
    let base = qdrant_url(db).await;
    let coll = collection_name(db).await;
    let threshold = min_score(db).await;
    let url = format!("{base}/collections/{coll}/points/search");

    let body = json!({
        "vector": vector,
        "limit": limit,
        "score_threshold": threshold,
        "with_payload": true,
        "with_vector": false,
    });

    let resp = nexus_http::build_client().post(&url).json(&body).send().await?;
    if !resp.status().is_success() {
        let msg = resp.text().await.unwrap_or_else(|_| "?".into());
        anyhow::bail!("Qdrant search fallita: {msg}");
    }

    let payload: Value = resp.json().await?;
    let hits = payload.get("result").and_then(Value::as_array).cloned().unwrap_or_default();

    // Filtra per scope (sicurezza)
    let user_str = user_id.to_string();
    let project_str = project_id.to_string();
    let mut results = Vec::new();
    for hit in hits {
        let p = hit.get("payload").cloned().unwrap_or_else(|| json!({}));
        let scope = p.get("scope").and_then(Value::as_str).unwrap_or("user");
        let sid_str = p.get("server_id").and_then(Value::as_str).unwrap_or("");

        // Verifica accesso scope
        let allowed = match scope {
            "global" => true,
            "project" => {
                // Richiede verifica DB sul project_id del server
                match sid_str.parse::<Uuid>() {
                    Ok(sid) => can_access_server(db, sid, user_id, project_id).await,
                    Err(_) => false,
                }
            }
            _ => {
                // scope "user": verifica owner
                match sid_str.parse::<Uuid>() {
                    Ok(sid) => can_access_server(db, sid, user_id, project_id).await,
                    Err(_) => {
                        // Fallback: confronta user_id nel payload se disponibile
                        p.get("user_id").and_then(Value::as_str) == Some(&user_str)
                    }
                }
            }
        };

        if !allowed {
            continue;
        }

        // Recupera input_schema dal DB (non memorizzato in Qdrant per risparmiare spazio)
        let sid: Option<Uuid> = sid_str.parse().ok();
        let tool_name = p.get("tool_name").and_then(Value::as_str).unwrap_or("").to_string();
        let server_name = p.get("server_name").and_then(Value::as_str).unwrap_or("").to_string();
        let description: Option<String> = p.get("description").and_then(Value::as_str).map(|s| s.to_string());
        let score = hit.get("score").and_then(Value::as_f64).unwrap_or(0.0);

        let input_schema: Value = if let Some(sid) = sid {
            sqlx::query_scalar::<_, Value>(
                "SELECT input_schema FROM mcp_server_tools WHERE server_id=$1 AND tool_name=$2",
            )
            .bind(sid)
            .bind(&tool_name)
            .fetch_optional(db)
            .await
            .unwrap_or(None)
            .unwrap_or_else(|| json!({}))
        } else {
            json!({})
        };

        let _ = user_str.as_str(); // suppress unused

        results.push(json!({
            "server_id": sid_str,
            "server_name": server_name,
            "tool_name": tool_name,
            "description": description,
            "input_schema": input_schema,
            "score": score,
            "match_type": "semantic",
        }));
    }

    Ok(results)
}

// ── Handler: nexus_mcp_tool_search ───────────────────────────────────────────

pub async fn handle_mcp_tool_search(
    db: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
    arguments: &Value,
) -> String {
    handle_mcp_tool_search_inner(db, None, user_id, project_id, arguments).await
}

pub async fn handle_mcp_tool_search_with_neural(
    db: &PgPool,
    neural: &NeuralCoreClient,
    user_id: Uuid,
    project_id: Uuid,
    arguments: &Value,
) -> String {
    handle_mcp_tool_search_inner(db, Some(neural), user_id, project_id, arguments).await
}

async fn handle_mcp_tool_search_inner(
    db: &PgPool,
    neural: Option<&NeuralCoreClient>,
    user_id: Uuid,
    project_id: Uuid,
    arguments: &Value,
) -> String {
    let query = parse_str(arguments.get("query")).unwrap_or_default();
    if query.is_empty() {
        return format_json(&json!({"error": "query richiesto"}));
    }
    let limit = parse_i64(arguments.get("limit"), 10).clamp(1, 50) as i64;

    // ── Tentativo 0: tool builtin Nexus (locale, ILIKE su AGENT_TOOLS_JSON) ──
    // Senza questo i tool nexus_extract_* / nexus_read_attachment / ecc. non
    // sarebbero scopribili e il modello finirebbe per chiamare mcp_tool_call
    // con server_id inventato. I builtin hanno SEMPRE server_id="builtin",
    // riconosciuto dal dispatcher in execute_agent_tool.
    let builtin_matches = search_builtin_tools(&query, limit as usize);

    // ── Tentativo 1: ricerca semantica (se neural disponibile) ────────────────
    if let Some(neural) = neural {
        match semantic_search(db, neural, &query, user_id, project_id, limit).await {
            Ok(results) if !results.is_empty() || !builtin_matches.is_empty() => {
                let mut merged = builtin_matches.clone();
                merged.extend(results.into_iter());
                return format_json(&json!({
                    "query": query,
                    "count": merged.len(),
                    "results": merged,
                    "search_type": "semantic+builtin",
                }));
            }
            Ok(_) => {
                tracing::debug!("mcp_tool_search: ricerca semantica vuota per '{}', fallback ILIKE", query);
            }
            Err(e) => {
                tracing::warn!("mcp_tool_search: ricerca semantica fallita ({}), fallback ILIKE", e);
            }
        }
    }

    // ── Fallback: ILIKE testuale ──────────────────────────────────────────────
    let like = format!("%{}%", query.replace('%', "").replace('_', ""));
    let rows = sqlx::query(
        r#"
        SELECT
          s.id          AS server_id,
          s.name        AS server_name,
          s.scope       AS scope,
          t.tool_name   AS tool_name,
          t.description AS description,
          t.input_schema AS input_schema
        FROM mcp_servers s
        JOIN mcp_server_tools t ON t.server_id = s.id
        WHERE s.enabled = true
          AND (
            s.scope = 'global'
            OR (s.scope = 'user' AND s.user_id = $1)
            OR (s.scope = 'project' AND s.project_id = $2)
          )
          AND (
            t.tool_name ILIKE $3
            OR COALESCE(t.description,'') ILIKE $3
            OR s.name ILIKE $3
          )
        ORDER BY s.scope DESC, s.name, t.tool_name
        LIMIT $4
        "#,
    )
    .bind(user_id)
    .bind(project_id)
    .bind(like)
    .bind(limit)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let external_results: Vec<Value> = rows
        .iter()
        .map(|r| {
            let server_id: Uuid = r.try_get("server_id").unwrap_or(Uuid::nil());
            let server_name: String = r.try_get("server_name").unwrap_or_default();
            let tool_name: String = r.try_get("tool_name").unwrap_or_default();
            let description: Option<String> = r.try_get("description").unwrap_or(None);
            let input_schema: Value = r.try_get::<Value, _>("input_schema").unwrap_or(json!({}));
            json!({
              "server_id": server_id.to_string(),
              "server_name": server_name,
              "tool_name": tool_name,
              "description": description,
              "input_schema": input_schema,
              "match_type": "text",
            })
        })
        .collect();

    // Merge builtin (gia' calcolati a inizio funzione) + esterni. I builtin
    // appaiono per primi: spesso sono la risposta giusta per task di estrazione
    // allegati / lettura risorse interne, e mettendoli in cima riduciamo il
    // rischio che il modello cerchi un MCP esterno equivalente.
    let mut merged = builtin_matches;
    merged.extend(external_results.into_iter());

    format_json(&json!({
      "query": query,
      "count": merged.len(),
      "results": merged,
      "search_type": "text+builtin",
    }))
}

/// Ricerca testuale (ILIKE) sui tool builtin esposti in AGENT_TOOLS_JSON.
/// Restituisce risultati con `server_id="builtin"` e schema completo, pronti
/// per essere invocati via `nexus_mcp_tool_call`. Il dispatcher in
/// `agent_tools::execute_agent_tool` riconosce questo server_id sentinella
/// e fa dispatch ricorsivo al tool builtin.
///
/// Match: substring case-insensitive su `name` e `description` (no regex,
/// no boundary parsing). Sufficiente per query come "estrai codice figma" o
/// "leggi pdf". L'ordine preserva quello di AGENT_TOOLS_JSON: i tool piu'
/// fondamentali sono dichiarati per primi.
fn search_builtin_tools(query: &str, limit: usize) -> Vec<Value> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let needle = query.to_ascii_lowercase();
    let tools_json: Value =
        match serde_json::from_str(crate::agent_tools::AGENT_TOOLS_JSON) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("search_builtin_tools: AGENT_TOOLS_JSON parse fallito: {}", e);
                return Vec::new();
            }
        };
    let arr = match tools_json.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };
    let mut out = Vec::with_capacity(limit.min(arr.len()));
    for t in arr.iter() {
        let name = t.get("name").and_then(Value::as_str).unwrap_or("");
        let description = t.get("description").and_then(Value::as_str).unwrap_or("");
        let input_schema = t.get("input_schema").cloned().unwrap_or(json!({}));
        if name.is_empty() {
            continue;
        }
        let haystack = format!("{}\n{}", name.to_ascii_lowercase(), description.to_ascii_lowercase());
        if !haystack.contains(&needle) {
            continue;
        }
        out.push(json!({
            "server_id": "builtin",
            "server_name": "Nexus builtin",
            "tool_name": name,
            "description": description,
            "input_schema": input_schema,
            "match_type": "builtin",
        }));
        if out.len() >= limit {
            break;
        }
    }
    out
}

// ── Handler: nexus_mcp_tool_call ─────────────────────────────────────────────

pub async fn handle_mcp_tool_call(
    db: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
    arguments: &Value,
) -> String {
    let server_id_str = parse_str(arguments.get("server_id"));
    let tool_name = parse_str(arguments.get("tool_name")).unwrap_or_default();
    let args = arguments.get("arguments").cloned().unwrap_or_else(|| json!({}));

    let Some(server_id_str) = server_id_str else {
        return format_json(&json!({"error": "server_id richiesto"}));
    };
    let Ok(server_id) = Uuid::parse_str(&server_id_str) else {
        return format_json(&json!({"error": "server_id non valido"}));
    };
    if tool_name.is_empty() {
        return format_json(&json!({"error": "tool_name richiesto"}));
    }

    if !can_access_server(db, server_id, user_id, project_id).await {
        return format_json(&json!({"error": "server non accessibile o disabilitato"}));
    }

    mcp_connectors::execute_mcp_tool(db, server_id, &tool_name, args).await
}

// ── Handler: nexus_mcp_tool_reindex ──────────────────────────────────────────

/// Rigenera l'indice semantico di tutti i tool MCP (o solo quelli non ancora indicizzati).
/// Richiede ruolo admin. Esegue in-process, restituisce un report.
pub async fn handle_mcp_tool_reindex(
    db: &PgPool,
    neural: Option<&NeuralCoreClient>,
    arguments: &Value,
) -> String {
    let force = arguments.get("force").and_then(Value::as_bool).unwrap_or(false);

    let Some(neural) = neural else {
        return format_json(&json!({"error": "embedder non disponibile (neural=None)"}));
    };

    // Crea/verifica collection
    if let Err(e) = ensure_mcp_tools_collection(db).await {
        return format_json(&json!({"error": format!("Qdrant collection: {e}")}));
    }

    // Carica tool da re-indicizzare
    let query = if force {
        "SELECT s.id AS server_id, s.name AS server_name, s.scope,
                t.tool_name, COALESCE(t.description,'') AS description
         FROM mcp_servers s
         JOIN mcp_server_tools t ON t.server_id = s.id
         WHERE s.enabled = true
         ORDER BY s.name, t.tool_name"
    } else {
        "SELECT s.id AS server_id, s.name AS server_name, s.scope,
                t.tool_name, COALESCE(t.description,'') AS description
         FROM mcp_servers s
         JOIN mcp_server_tools t ON t.server_id = s.id
         WHERE s.enabled = true AND t.embedded_at IS NULL
         ORDER BY s.name, t.tool_name"
    };

    let rows = sqlx::query(query).fetch_all(db).await.unwrap_or_default();
    let total = rows.len();
    let mut indexed = 0usize;
    let mut errors = 0usize;

    for row in &rows {
        let server_id: Uuid = row.try_get("server_id").unwrap_or(Uuid::nil());
        let server_name: String = row.try_get("server_name").unwrap_or_default();
        let scope: String = row.try_get("scope").unwrap_or_else(|_| "user".into());
        let tool_name: String = row.try_get("tool_name").unwrap_or_default();
        let description: String = row.try_get("description").unwrap_or_default();

        match index_tool(db, neural, server_id, &server_name, &tool_name, &description, &scope).await {
            Ok(()) => indexed += 1,
            Err(e) => {
                tracing::warn!("reindex: errore su {}/{}: {}", server_name, tool_name, e);
                errors += 1;
            }
        }
    }

    format_json(&json!({
        "total_processed": total,
        "indexed": indexed,
        "errors": errors,
        "force": force,
    }))
}

// ── Formatter ─────────────────────────────────────────────────────────────────

fn format_json(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}
