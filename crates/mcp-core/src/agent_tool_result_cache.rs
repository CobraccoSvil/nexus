//! Cache cross-turn dei tool_result (ADR 0016 Fase A.5).
//!
//! Backing Postgres (tabella `agent_tool_result_cache`, mig 0287). Cache key
//! = sha256(tool_name + canonical_args_json). Stesso tool con stessi args
//! ritorna il payload precedente invece di re-eseguire, con TTL configurabile
//! (default 30 min) e skiplist per tool con side-effect (run_command, write_file...).
//!
//! Lookup hot path: SELECT per PK + UPDATE hit_count. Pulizia opportunistica:
//! cleanup_expired() rimuove le righe scadute (chiamato dal worker periodico).

use anyhow::{Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

/// Risultato lookup: payload + ref id se la chiave esiste e non e' scaduta.
pub struct CacheHit {
    pub payload: String,
    pub cache_key: String,
    pub age_seconds: i64,
}

/// Settings cache (cache 60s a livello processo).
#[derive(Debug, Clone)]
pub struct CacheConfig {
    pub enabled: bool,
    pub ttl_seconds: u64,
    pub skip_for: Vec<String>,
}

impl CacheConfig {
    pub async fn load(db: &PgPool) -> Self {
        let enabled = read_bool(db, "agent.tools.result_cache_enabled", true).await;
        let ttl_seconds = read_u64(db, "agent.tools.result_cache_ttl_seconds", 1800).await;
        let skip_csv = read_text(
            db,
            "agent.tools.result_cache_skip_for",
            "run_command,write_file,edit_file,delete_file",
        )
        .await;
        let skip_for: Vec<String> = skip_csv
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Self {
            enabled,
            ttl_seconds,
            skip_for,
        }
    }

    pub fn should_cache(&self, tool_name: &str) -> bool {
        self.enabled && !self.skip_for.iter().any(|s| s == tool_name)
    }
}

async fn read_bool(db: &PgPool, key: &str, default: bool) -> bool {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .map(|v| v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(default)
}

async fn read_u64(db: &PgPool, key: &str, default: u64) -> u64 {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

async fn read_text(db: &PgPool, key: &str, default: &str) -> String {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Calcola la cache key per (tool_name, args). Canonicalizza l'ordine chiavi
/// del JSON via serde (Map BTree-like a tempo di build) per evitare miss su
/// permutazioni identiche.
pub fn make_cache_key(tool_name: &str, args: &Value) -> String {
    let canonical = serde_json::to_string(&canonical_value(args)).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(tool_name.as_bytes());
    hasher.update(b"\x00");
    hasher.update(canonical.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Rende un Value canonico: gli Object diventano BTreeMap-equivalenti via
/// to_value che ordina per chiave (serde_json mantiene insertion order, qui
/// forziamo sort manualmente).
fn canonical_value(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .iter()
                .map(|(k, vv)| (k.clone(), canonical_value(vv)))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let mut out = serde_json::Map::new();
            for (k, vv) in entries {
                out.insert(k, vv);
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(canonical_value).collect()),
        other => other.clone(),
    }
}

/// Cerca payload cached per (tool_name, args). None se miss o scaduto.
pub async fn lookup(db: &PgPool, tool_name: &str, args: &Value) -> Option<CacheHit> {
    let key = make_cache_key(tool_name, args);
    let row = sqlx::query(
        r#"
        SELECT payload, EXTRACT(EPOCH FROM (NOW() - created_at))::bigint AS age_s
        FROM agent_tool_result_cache
        WHERE cache_key = $1 AND expires_at > NOW()
        "#,
    )
    .bind(&key)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()?;

    let payload: String = sqlx::Row::try_get(&row, "payload").ok()?;
    let age: i64 = sqlx::Row::try_get(&row, "age_s").unwrap_or(0);

    // Bump hit_count + last_hit_at (best-effort, asincrono via tokio::spawn).
    let db_clone = db.clone();
    let key_clone = key.clone();
    tokio::spawn(async move {
        let _ = sqlx::query(
            "UPDATE agent_tool_result_cache SET hit_count = hit_count + 1, last_hit_at = NOW() WHERE cache_key = $1",
        )
        .bind(&key_clone)
        .execute(&db_clone)
        .await;
    });

    Some(CacheHit {
        payload,
        cache_key: key,
        age_seconds: age,
    })
}

/// Memorizza payload (overwrite se chiave esiste, refresh expires_at).
pub async fn store(
    db: &PgPool,
    tool_name: &str,
    args: &Value,
    payload: &str,
    ttl_seconds: u64,
) -> Result<()> {
    let key = make_cache_key(tool_name, args);
    let bytes = payload.len() as i32;
    sqlx::query(
        r#"
        INSERT INTO agent_tool_result_cache
            (cache_key, tool_name, payload, payload_bytes, expires_at)
        VALUES ($1, $2, $3, $4, NOW() + ($5 || ' seconds')::interval)
        ON CONFLICT (cache_key) DO UPDATE SET
            payload      = EXCLUDED.payload,
            payload_bytes = EXCLUDED.payload_bytes,
            expires_at   = EXCLUDED.expires_at
        "#,
    )
    .bind(&key)
    .bind(tool_name)
    .bind(payload)
    .bind(bytes)
    .bind(ttl_seconds.to_string())
    .execute(db)
    .await
    .context("INSERT agent_tool_result_cache")?;
    Ok(())
}

/// Pulizia righe scadute (chiamare periodicamente).
pub async fn cleanup_expired(db: &PgPool) -> Result<u64> {
    let res = sqlx::query("DELETE FROM agent_tool_result_cache WHERE expires_at <= NOW()")
        .execute(db)
        .await
        .context("DELETE expired cache")?;
    Ok(res.rows_affected())
}

/// Worker periodico (15 min) di pulizia cache scadute. Non blocca l'avvio,
/// loggato solo a INFO/WARN. Si avvia con `tokio::spawn` da main.rs.
pub fn start_cleanup_worker(db: PgPool) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(900));
        interval.tick().await; // skip primo tick immediato
        loop {
            interval.tick().await;
            match cleanup_expired(&db).await {
                Ok(n) if n > 0 => {
                    tracing::info!("agent_tool_result_cache: pulite {} righe scadute", n)
                }
                Ok(_) => tracing::debug!("agent_tool_result_cache: cleanup, 0 righe"),
                Err(e) => tracing::warn!("agent_tool_result_cache cleanup fallito: {e}"),
            }
        }
    });
    tracing::info!("agent_tool_result_cache: cleanup worker avviato (15m interval)");
}
