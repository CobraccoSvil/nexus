//! Cache LRU+TTL per deduplicare letture allegati ripetitive.
//!
//! Vedi ADR 0012 (FIX 2). Il loop osservato in produzione era:
//! il modello chiamava `nexus_read_archive_entry` 4+ volte con offset
//! progressivi sullo stesso `canvas.fig` binario, saturando il context
//! window. Questa cache:
//!
//! 1. Deduplica chiamate identiche (stessa attachment_id + entry_path +
//!    offset + length + encoding) servendo dalla memoria invece di rileggere
//!    il file. Il payload e' gia' serializzato in JSON (stringa pronta da
//!    ritornare al modello).
//! 2. Quando lo stesso pattern viene servito >= 2 volte, inietta un campo
//!    `from_cache`+`hint` per suggerire al modello di cambiare strategia
//!    (passare a un tool di estrazione strutturata invece di re-leggere
//!    chunk binari).
//!
//! TTL configurabile via setting DB `agent.attachment.read_cache_ttl_seconds`
//! (default 300 secondi). Capacita' fissa 256 entry (LRU evict).

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lru::LruCache;
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use uuid::Uuid;

use super::attachment_settings;
use sqlx::PgPool;

/// Tipo di lettura per discriminare i namespace di cache.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum ReadKind {
    /// `nexus_read_attachment`: lettura range diretta dell'allegato.
    Attachment,
    /// `nexus_read_archive_entry`: lettura entry dentro archivio.
    ArchiveEntry,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct ReadCacheKey {
    pub attachment_id: Uuid,
    pub kind: ReadKind,
    pub entry_path: Option<String>,
    pub offset: u64,
    pub length: u64,
    pub encoding: String,
}

#[derive(Debug, Clone)]
struct CachedEntry {
    /// Payload JSON gia' serializzato (string pronta da ritornare al modello).
    payload: String,
    /// Numero di volte che questa entry e' stata servita (1 = prima scrittura).
    served_count: u32,
    /// Quando scade (Instant + TTL).
    expires_at: Instant,
}

const CAPACITY: usize = 256;

static READ_CACHE: Lazy<Arc<RwLock<LruCache<ReadCacheKey, CachedEntry>>>> = Lazy::new(|| {
    let cap = NonZeroUsize::new(CAPACITY).expect("capacita' read_cache > 0");
    Arc::new(RwLock::new(LruCache::new(cap)))
});

/// Ritorna il TTL letto da DB (cache 60s gia' applicata dal modulo
/// `attachment_settings`).
async fn cache_ttl(db: &PgPool) -> Duration {
    let limits = attachment_settings::current(db).await;
    Duration::from_secs(limits.read_cache_ttl_seconds.max(1) as u64)
}

/// Cerca la chiave nella cache. Se hit valido, incrementa `served_count` e
/// ritorna il payload arricchito con `from_cache`/`served_count`/`hint`.
/// Altrimenti chiama `compute` (lettura raw), memorizza il risultato e ritorna
/// il payload originale.
pub async fn get_or_compute<F, Fut>(db: &PgPool, key: ReadCacheKey, compute: F) -> String
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = String>,
{
    // 1. Hit check con write lock (dobbiamo incrementare served_count).
    {
        let mut guard = READ_CACHE.write().await;
        if let Some(entry) = guard.get_mut(&key) {
            if Instant::now() < entry.expires_at {
                entry.served_count = entry.served_count.saturating_add(1);
                let served = entry.served_count;
                let base_payload = entry.payload.clone();
                drop(guard);
                return annotate_cached(base_payload, served);
            }
            // Scaduto: rimuovi e cadi su compute.
            guard.pop(&key);
        }
    }

    // 2. Miss: calcola e memorizza.
    let payload = compute().await;
    let ttl = cache_ttl(db).await;
    let entry = CachedEntry {
        payload: payload.clone(),
        served_count: 1,
        expires_at: Instant::now() + ttl,
    };
    let mut guard = READ_CACHE.write().await;
    guard.put(key, entry);
    payload
}

/// Aggiunge i campi `from_cache`/`served_count`/`hint` al payload JSON
/// originale quando la stessa richiesta e' gia' stata servita >= 2 volte.
fn annotate_cached(payload: String, served_count: u32) -> String {
    if served_count < 2 {
        return payload;
    }
    let mut value: Value = match serde_json::from_str(&payload) {
        Ok(v) => v,
        Err(_) => return payload,
    };
    if !value.is_object() {
        return payload;
    }
    let obj = value.as_object_mut().expect("valore JSON oggetto");
    obj.insert("from_cache".into(), json!(true));
    obj.insert("served_count".into(), json!(served_count));
    obj.insert(
        "hint".into(),
        json!(format!(
            "Questa esatta richiesta e' gia' stata servita {} volte.              Considera di passare a una entry diversa o di usare un tool di              estrazione strutturata (vedi extraction_tools/next_action_recommended              nell'inspector): leggere chunk binari a offset crescenti satura il              context window senza aggiungere informazione utile.",
            served_count
        )),
    );
    value.to_string()
}

#[cfg(test)]
pub async fn _reset_for_tests() {
    let mut guard = READ_CACHE.write().await;
    guard.clear();
}
