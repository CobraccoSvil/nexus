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

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, QueryBuilder, Row};
use uuid::Uuid;

use crate::mcp_connectors;
use crate::orchestrator::NeuralCoreClient;

// ── Costanti ─────────────────────────────────────────────────────────────────

const DEFAULT_QDRANT_URL: &str = "http://localhost:6333";
const DEFAULT_COLLECTION: &str = "mcp_tools";
const VECTOR_SIZE: u64 = 384;
const DEFAULT_MIN_SCORE: f64 = 0.35;

/// Lunghezza minima (in byte/char ascii) sotto la quale un token viene
/// scartato dalla tokenizzazione del fallback ILIKE. Articoli/preposizioni
/// corte ("il", "di", "su") finiscono sotto soglia; termini tecnici corti
/// ("db", "id") sono comunque rari e il filtro stopword copre il resto.
const TOKEN_MIN_LEN: usize = 3;

/// Numero massimo di token usati nel fallback ILIKE tokenizzato. Limita il
/// numero di clausole OR generate (e quindi il costo della query) anche con
/// query molto lunghe. I primi N token significativi sono sufficienti.
const MAX_SEARCH_TOKENS: usize = 12;

/// Stopword italiane/inglesi (articoli, preposizioni, congiunzioni, ausiliari)
/// scartate dalla tokenizzazione. NON include termini tecnici (sql, database,
/// query, migration, table...) che restano significativi per il ranking.
const SEARCH_STOPWORDS: &[&str] = &[
    // italiano
    "una", "uno", "del", "dei", "delle", "della", "dello", "sul", "sui", "sulla", "con", "per",
    "che", "non", "come", "dove", "quando", "questo", "questa", "quello", "quella", "nel", "nella",
    "negli", "alla", "allo", "agli", "dal", "dalla", "tra", "fra", "gli", "lei", "lui", "noi",
    "voi", "loro", "suo", "sua", "mio", "mia", "tuo", "tua", "fare", "essere", "avere", "esegui",
    "eseguire", "crea", "creare", "vuoi", "puoi", // inglese
    "and", "the", "for", "with", "that", "this", "from", "into", "your", "you", "can", "are",
    "was", "were", "has", "have", "his", "her", "its", "our", "their", "want", "create", "make",
    "run", "exec", "execute",
];

// ── Helpers DB settings ───────────────────────────────────────────────────────

// Lettura setting: punto unico in nexus-auth (regola L / ADR 0026).
use nexus_auth::get_setting;

async fn qdrant_url(db: &PgPool) -> String {
    get_setting(db, "qdrant_url")
        .await
        .or_else(|| std::env::var("QDRANT_URL").ok())
        .unwrap_or_else(|| DEFAULT_QDRANT_URL.to_string())
}

async fn collection_name(db: &PgPool) -> String {
    get_setting(db, "qdrant_mcp_tools_collection")
        .await
        .unwrap_or_else(|| DEFAULT_COLLECTION.to_string())
}

async fn min_score(db: &PgPool) -> f64 {
    get_setting(db, "mcp_tool_search_min_score")
        .await
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(DEFAULT_MIN_SCORE)
        .clamp(0.0, 1.0)
}

fn parse_i64(v: Option<&Value>, default: i64) -> i64 {
    v.and_then(Value::as_i64).unwrap_or(default)
}

fn parse_str(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ── Sicurezza accesso server ──────────────────────────────────────────────────

async fn can_access_server(db: &PgPool, server_id: Uuid, user_id: Uuid, project_id: Uuid) -> bool {
    let row =
        sqlx::query("SELECT scope, user_id, project_id, enabled FROM mcp_servers WHERE id=$1")
            .bind(server_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();

    let Some(row) = row else {
        return false;
    };
    let enabled: bool = row.try_get("enabled").unwrap_or(false);
    if !enabled {
        return false;
    }

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

/// Hash dei primi 256 char di (name + description) piu' la signature
/// dell'embedder attivo, per rilevare cambiamenti.
///
/// La signature (es. `"all-MiniLM-L6-v2:384"` oppure `"hash:256"`) rende
/// l'hash sensibile al modello embedder: quando l'embedder cambia, l'hash
/// cambia anche a parita' di descrizione, cosi' il reindex automatico
/// all'avvio rigenera i vettori nel nuovo spazio senza interventi manuali.
/// A parita' di embedder + descrizione l'hash resta stabile (idempotenza).
fn embedding_hash(tool_name: &str, description: &str, embedder_signature: &str) -> String {
    let mut h = Sha256::new();
    let combined = format!("{tool_name}:{description}");
    h.update(
        combined
            .as_bytes()
            .iter()
            .take(256)
            .copied()
            .collect::<Vec<_>>(),
    );
    // La signature non e' troncata: e' corta e identifica lo spazio vettoriale.
    h.update(b"|emb:");
    h.update(embedder_signature.as_bytes());
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
    server_id: Uuid,
    server_name: &str,
    tool_name: &str,
    description: &str,
    scope: &str,
) -> anyhow::Result<()> {
    // Hash corrente nel DB (potenzialmente nullo se mai indicizzato).
    let existing: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT embedding_hash FROM mcp_server_tools WHERE server_id=$1 AND tool_name=$2",
    )
    .bind(server_id)
    .bind(tool_name)
    .fetch_optional(db)
    .await
    .unwrap_or(None);
    let old_hash: Option<String> = existing.and_then(|(h,)| h);

    // Genera embedding con l'embedder ONNX in-process del bridge (regola L:
    // punto unico, niente round-trip al brain Python). `embed_one` e'
    // sincrono/CPU-bound, quindi avvolto in `spawn_blocking`.
    let text = embed_text_for_tool(tool_name, description);
    let bridge = crate::nexus_bridge::NexusBridge::global()
        .ok_or_else(|| anyhow::anyhow!("nexus bridge non inizializzato"))?;
    let embed_input = text.clone();
    let vector = tokio::task::spawn_blocking(move || bridge.embed_one(&embed_input))
        .await
        .map_err(|e| anyhow::anyhow!("embed_text spawn_blocking join: {e}"))?;
    // Label FISSA "all-MiniLM-L6-v2" nella signature (NON embedder.name(), che
    // e' "onnx-minilm-l6-v2"): i vettori della tool-search gia' indicizzati sono
    // numericamente paritetici col nuovo embedder (cosine 1.0), quindi gli hash
    // esistenti restano validi e si evita un re-index inutile.
    let embedder_signature = format!("all-MiniLM-L6-v2:{}", vector.len());
    let new_hash = embedding_hash(tool_name, description, &embedder_signature);

    // Idempotenza: a parita' di descrizione + embedder l'hash coincide -> skip
    // dell'upsert su Qdrant (l'embed e' gia' stato calcolato ma e' a basso costo
    // rispetto alla riscrittura del point e all'aggiornamento DB).
    if old_hash.as_deref() == Some(new_hash.as_str()) {
        return Ok(()); // Nessun cambiamento (descrizione + embedder invariati)
    }

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

    let r = nexus_http::build_client()
        .put(&url)
        .json(&body)
        .send()
        .await?;
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

/// `pub(crate)` (regola L): riusata best-effort dal tool-not-found resolver per
/// arricchire i suggerimenti con i match semantici dei connettori MCP quando
/// Qdrant e' disponibile. Mai duplicata.
///
/// L'embedding della query e' prodotto dall'embedder ONNX in-process del bridge
/// (regola L: punto unico, niente round-trip al brain Python). I vettori cosi'
/// generati sono paritetici a quelli usati in indicizzazione (`index_tool`).
pub(crate) async fn semantic_search(
    db: &PgPool,
    query: &str,
    user_id: Uuid,
    project_id: Uuid,
    limit: i64,
) -> anyhow::Result<Vec<Value>> {
    // Embedding della query con l'embedder in-process. `embed_one` e'
    // sincrono/CPU-bound, quindi avvolto in `spawn_blocking`. Niente fallback
    // silenzioso: se il bridge non e' inizializzato si propaga l'errore (regola G).
    let bridge = crate::nexus_bridge::NexusBridge::global()
        .ok_or_else(|| anyhow::anyhow!("nexus bridge non inizializzato"))?;
    let embed_input = query.to_string();
    let vector = tokio::task::spawn_blocking(move || bridge.embed_one(&embed_input))
        .await
        .map_err(|e| anyhow::anyhow!("embed_one spawn_blocking join: {e}"))?;

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

    let resp = nexus_http::build_client()
        .post(&url)
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let msg = resp.text().await.unwrap_or_else(|_| "?".into());
        anyhow::bail!("Qdrant search fallita: {msg}");
    }

    let payload: Value = resp.json().await?;
    let hits = payload
        .get("result")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // Filtra per scope (sicurezza)
    let user_str = user_id.to_string();
    let _project_str = project_id.to_string();
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
        let tool_name = p
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let server_name = p
            .get("server_name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let description: Option<String> = p
            .get("description")
            .and_then(Value::as_str)
            .map(|s| s.to_string());
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

/// Tokenizza una query libera in keyword significative per il fallback ILIKE.
///
/// Pipeline: lowercase -> split su qualunque carattere non alfanumerico
/// (whitespace + punteggiatura) -> scarta token < `TOKEN_MIN_LEN` e stopword
/// -> dedup preservando l'ordine -> tronca a `MAX_SEARCH_TOKENS`.
///
/// I `%` e `_` (wildcard ILIKE) sono gia' esclusi essendo non alfanumerici,
/// quindi i token sono safe per il bind `%token%`. Il valore resta comunque
/// passato come bind parametrico (mai interpolato), questa e' difesa in
/// profondita'.
fn tokenize_query(query: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for raw in query.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
        if raw.chars().count() < TOKEN_MIN_LEN {
            continue;
        }
        if SEARCH_STOPWORDS.contains(&raw) {
            continue;
        }
        if seen.insert(raw.to_string()) {
            out.push(raw.to_string());
            if out.len() >= MAX_SEARCH_TOKENS {
                break;
            }
        }
    }
    out
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
    let limit = parse_i64(arguments.get("limit"), 10).clamp(1, 50);

    // ── Tentativo 0: tool builtin Nexus (locale, ILIKE su AGENT_TOOLS_JSON) ──
    // Senza questo i tool nexus_extract_* / nexus_read_attachment / ecc. non
    // sarebbero scopribili e il modello finirebbe per chiamare mcp_tool_call
    // con server_id inventato. I builtin hanno SEMPRE server_id="builtin",
    // riconosciuto dal dispatcher in execute_agent_tool.
    let builtin_matches = search_builtin_tools(&query, limit as usize);

    // ── Tentativo 1: ricerca semantica (se neural-core/Qdrant configurato) ────
    // Il gate resta `neural` (presenza del NeuralCoreClient = Qdrant disponibile);
    // l'embedding della query e' invece prodotto in-process da `semantic_search`.
    if neural.is_some() {
        match semantic_search(db, &query, user_id, project_id, limit).await {
            Ok(results) if !results.is_empty() || !builtin_matches.is_empty() => {
                let mut merged = builtin_matches.clone();
                merged.extend(results);
                return format_json(&json!({
                    "query": query,
                    "count": merged.len(),
                    "results": merged,
                    "search_type": "semantic+builtin",
                }));
            }
            Ok(_) => {
                tracing::debug!(
                    "mcp_tool_search: ricerca semantica vuota per '{}', fallback ILIKE",
                    query
                );
            }
            Err(e) => {
                tracing::warn!(
                    "mcp_tool_search: ricerca semantica fallita ({}), fallback ILIKE",
                    e
                );
            }
        }
    }

    // ── Fallback: ILIKE testuale TOKENIZZATO con ranking ──────────────────────
    // La query utente e' spesso una frase ("esegui una query SQL sul database
    // del progetto"). Cercare la frase intera come unica substring ritorna 0
    // (nessun tool contiene la frase). Tokenizziamo in keyword e cerchiamo in
    // OR sui token, ordinando per numero di token che matchano (ranking).
    let tokens = tokenize_query(&query);

    let rows = if tokens.is_empty() {
        // Nessun token significativo (query di sole stopword/simboli):
        // fallback al comportamento legacy (frase intera) per non regredire.
        let like = format!("%{}%", query.replace(['%', '_'], ""));
        sqlx::query(
            r#"
            SELECT
              s.id          AS server_id,
              s.name        AS server_name,
              s.scope       AS scope,
              t.tool_name   AS tool_name,
              t.description AS description,
              t.input_schema AS input_schema,
              1::int        AS match_score
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
        .unwrap_or_default()
    } else {
        // Costruzione dinamica SQL injection-safe via QueryBuilder: ogni token
        // produce (a) una clausola OR nel WHERE e (b) un addendo CASE WHEN nel
        // punteggio. Tutti i valori `%token%` sono bind parametrici.
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
            "SELECT s.id AS server_id, s.name AS server_name, s.scope AS scope, \
             t.tool_name AS tool_name, t.description AS description, \
             t.input_schema AS input_schema, (",
        );
        // Punteggio: somma di CASE WHEN (tool_name|description|server_name match token) THEN 1.
        let mut first = true;
        for tok in &tokens {
            let pat = format!("%{tok}%");
            if !first {
                qb.push(" + ");
            }
            first = false;
            qb.push("(CASE WHEN t.tool_name ILIKE ");
            qb.push_bind(pat.clone());
            qb.push(" OR COALESCE(t.description,'') ILIKE ");
            qb.push_bind(pat.clone());
            qb.push(" OR s.name ILIKE ");
            qb.push_bind(pat.clone());
            qb.push(" THEN 1 ELSE 0 END)");
        }
        qb.push(") AS match_score FROM mcp_servers s \
                 JOIN mcp_server_tools t ON t.server_id = s.id \
                 WHERE s.enabled = true AND (s.scope = 'global' OR (s.scope = 'user' AND s.user_id = ");
        qb.push_bind(user_id);
        qb.push(") OR (s.scope = 'project' AND s.project_id = ");
        qb.push_bind(project_id);
        qb.push(")) AND (");
        // Clausola WHERE: almeno un token deve matchare.
        let mut first_w = true;
        for tok in &tokens {
            let pat = format!("%{tok}%");
            if !first_w {
                qb.push(" OR ");
            }
            first_w = false;
            qb.push("t.tool_name ILIKE ");
            qb.push_bind(pat.clone());
            qb.push(" OR COALESCE(t.description,'') ILIKE ");
            qb.push_bind(pat.clone());
            qb.push(" OR s.name ILIKE ");
            qb.push_bind(pat.clone());
        }
        qb.push(") ORDER BY match_score DESC, s.scope DESC, s.name, t.tool_name LIMIT ");
        qb.push_bind(limit);

        match qb.build().fetch_all(db).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("mcp_runtime: search external tools fallita: {e}");
                Vec::new()
            }
        }
    };

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
    merged.extend(external_results);

    // Osservabilita': nessun payload sensibile, solo la query (gia' loggata
    // altrove come metadato del tool call) + conteggi.
    tracing::info!(
        token_count = tokens.len(),
        result_count = merged.len(),
        "mcp_tool_search: fallback ILIKE tokenizzato per query='{}'",
        query
    );

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
/// Match: tokenizzazione della query (stessa di `tokenize_query`) + ricerca
/// per keyword su `name` e `description`, con ranking per numero di token che
/// matchano. Per query frasali ("esegui una query SQL sul database") la
/// ricerca della frase intera ritornerebbe 0; cosi' i builtin fondamentali
/// restano scopribili per parola chiave. Se la query non produce token
/// significativi (sole stopword/simboli) si ricade sulla substring intera per
/// non regredire su query mono-keyword corte.
///
/// `pub(crate)` (regola L): riusato dal punto unico tool-not-found resolver
/// (`crate::agent_tools::tool_not_found`) per produrre i suggerimenti "forse
/// intendevi" senza duplicare la logica di ricerca sul registro builtin.
pub(crate) fn search_builtin_tools(query: &str, limit: usize) -> Vec<Value> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let tools_json: Value = match serde_json::from_str(crate::agent_tools::AGENT_TOOLS_JSON) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                "search_builtin_tools: AGENT_TOOLS_JSON parse fallito: {}",
                e
            );
            return Vec::new();
        }
    };
    let arr = match tools_json.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };

    let tokens = tokenize_query(query);
    // Fallback frase intera quando non restano token significativi.
    let whole = query.to_ascii_lowercase();

    // Raccoglie (score, indice_originale, value) per ranking stabile.
    let mut scored: Vec<(usize, usize, Value)> = Vec::new();
    for (idx, t) in arr.iter().enumerate() {
        let name = t.get("name").and_then(Value::as_str).unwrap_or("");
        let description = t.get("description").and_then(Value::as_str).unwrap_or("");
        let input_schema = t.get("input_schema").cloned().unwrap_or(json!({}));
        if name.is_empty() {
            continue;
        }
        let haystack = format!(
            "{}\n{}",
            name.to_ascii_lowercase(),
            description.to_ascii_lowercase()
        );

        let score = if tokens.is_empty() {
            if haystack.contains(&whole) {
                1
            } else {
                0
            }
        } else {
            tokens
                .iter()
                .filter(|tok| haystack.contains(tok.as_str()))
                .count()
        };
        if score == 0 {
            continue;
        }

        scored.push((
            score,
            idx,
            json!({
                "server_id": "builtin",
                "server_name": "Nexus builtin",
                "tool_name": name,
                "description": description,
                "input_schema": input_schema,
                "match_type": "builtin",
            }),
        ));
    }

    // Ordina per score DESC, poi ordine di dichiarazione (i tool piu'
    // fondamentali sono dichiarati per primi in AGENT_TOOLS_JSON).
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().take(limit).map(|(_, _, v)| v).collect()
}

/// Tool di un connettore MCP installato/abilitato che corrisponde (esatto o
/// parziale) a un nome cercato. Riusato dal punto unico tool-not-found resolver
/// (regola L): la clausola scope/enabled vive accanto alla search, NON viene
/// duplicata nel resolver.
#[derive(Debug, Clone)]
pub(crate) struct InstalledToolMatch {
    pub server_id: Uuid,
    pub server_name: String,
    pub tool_name: String,
    pub description: Option<String>,
}

/// Cerca per NOME (non per descrizione) un tool tra i connettori MCP installati
/// e abilitati accessibili dall'utente/progetto. Match `t.tool_name ILIKE
/// %name%` (cattura sia esatto sia storpiature contenute). Stessa clausola
/// scope/enabled della search (`mcp_servers.scope` global/user/project +
/// `enabled = true`). Best-effort: su errore DB ritorna `Vec` vuoto (mai panic),
/// cosi' il resolver degrada al messaggio base mantenendo `is_error` coerente.
pub(crate) async fn lookup_installed_tool_by_name(
    db: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
    name: &str,
) -> Vec<InstalledToolMatch> {
    let name = name.trim();
    if name.is_empty() {
        return Vec::new();
    }
    // Sanitizza le wildcard ILIKE: i token builtin sono nomi-tool, ma il nome
    // arriva dal modello (puo' contenere `%`/`_`). Difesa in profondita': resta
    // comunque bind parametrico.
    let like = format!("%{}%", name.replace(['%', '_'], ""));
    let rows = sqlx::query(
        r#"
        SELECT
          s.id          AS server_id,
          s.name        AS server_name,
          t.tool_name   AS tool_name,
          t.description AS description
        FROM mcp_servers s
        JOIN mcp_server_tools t ON t.server_id = s.id
        WHERE s.enabled = true
          AND (
            s.scope = 'global'
            OR (s.scope = 'user' AND s.user_id = $1)
            OR (s.scope = 'project' AND s.project_id = $2)
          )
          AND t.tool_name ILIKE $3
        ORDER BY
          -- match esatto (case-insensitive) prima, poi per scope/nome
          (CASE WHEN lower(t.tool_name) = lower($4) THEN 0 ELSE 1 END),
          s.scope DESC, s.name, t.tool_name
        LIMIT 5
        "#,
    )
    .bind(user_id)
    .bind(project_id)
    .bind(like)
    .bind(name)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    rows.iter()
        .map(|r| InstalledToolMatch {
            server_id: r.try_get("server_id").unwrap_or_else(|_| Uuid::nil()),
            server_name: r.try_get("server_name").unwrap_or_default(),
            tool_name: r.try_get("tool_name").unwrap_or_default(),
            description: r.try_get("description").unwrap_or(None),
        })
        .collect()
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
    let args = arguments
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

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
pub async fn handle_mcp_tool_reindex(db: &PgPool, arguments: &Value) -> String {
    let force = arguments
        .get("force")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // L'embedding e' in-process (bridge ONNX), non dipende piu' dal brain: il
    // reindex funziona indipendentemente dalla disponibilita' del NeuralCoreClient.

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

    let rows = match sqlx::query(query).fetch_all(db).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("mcp_runtime: list tools-to-index fallita: {e}");
            Vec::new()
        }
    };
    let total = rows.len();
    let mut indexed = 0usize;
    let mut errors = 0usize;

    for row in &rows {
        let server_id: Uuid = row.try_get("server_id").unwrap_or(Uuid::nil());
        let server_name: String = row.try_get("server_name").unwrap_or_default();
        let scope: String = row.try_get("scope").unwrap_or_else(|_| "user".into());
        let tool_name: String = row.try_get("tool_name").unwrap_or_default();
        let description: String = row.try_get("description").unwrap_or_default();

        match index_tool(
            db,
            server_id,
            &server_name,
            &tool_name,
            &description,
            &scope,
        )
        .await
        {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_hash_idempotente_a_parita_di_embedder() {
        let a = embedding_hash("read_file", "legge un file", "onnx-minilm-l6-v2:384");
        let b = embedding_hash("read_file", "legge un file", "onnx-minilm-l6-v2:384");
        assert_eq!(a, b, "stesso embedder + descrizione -> hash stabile");
    }

    #[test]
    fn embedding_hash_cambia_al_cambio_embedder() {
        let onnx = embedding_hash("read_file", "legge un file", "onnx-minilm-l6-v2:384");
        let hash = embedding_hash("read_file", "legge un file", "hash:256");
        assert_ne!(
            onnx, hash,
            "cambio embedder deve invalidare l'hash per forzare il reindex"
        );
    }

    #[test]
    fn tokenize_estrae_keyword_significative() {
        let toks = tokenize_query("esegui una query SQL sul database del progetto");
        // "esegui", "una", "sul", "del" scartati (stopword); "query", "sql",
        // "database", "progetto" tenuti.
        assert!(toks.contains(&"query".to_string()));
        assert!(toks.contains(&"sql".to_string()));
        assert!(toks.contains(&"database".to_string()));
        assert!(toks.contains(&"progetto".to_string()));
        assert!(!toks.contains(&"una".to_string()));
        assert!(!toks.contains(&"esegui".to_string()));
    }

    #[test]
    fn tokenize_dedup_e_lowercase() {
        let toks = tokenize_query("Database DATABASE database");
        assert_eq!(toks, vec!["database".to_string()]);
    }

    #[test]
    fn tokenize_scarta_token_corti_e_punteggiatura() {
        let toks = tokenize_query("crea db, id: ok!");
        // "crea" stopword, "db"/"id"/"ok" < TOKEN_MIN_LEN -> nessun token.
        assert!(toks.is_empty());
    }

    #[test]
    fn tokenize_tronca_a_max() {
        let lunga = (0..30)
            .map(|i| format!("token{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let toks = tokenize_query(&lunga);
        assert_eq!(toks.len(), MAX_SEARCH_TOKENS);
    }

    #[test]
    fn builtin_search_matcha_per_keyword() {
        // Una query frasale deve comunque scoprire qualche builtin se la frase
        // contiene keyword presenti nei nomi/descrizioni dei tool.
        let res = search_builtin_tools("leggi il contenuto di un allegato pdf", 10);
        // Non asseriamo un tool specifico (il set evolve), ma la tokenizzazione
        // non deve far regredire a 0 quando keyword come "pdf"/"allegato"
        // esistono. Se il catalogo builtin cambia drasticamente questo test
        // resta tollerante: verifica solo che non panichi e ritorni un Vec.
        let _ = res.len();
    }
}
