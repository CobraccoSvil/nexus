//! RuVectorStore — interfaccia ad alto livello per una singola collection.
//!
//! Wrappa `HnswDb` con persistenza opzionale su PostgreSQL.
//! Tutte le operazioni di scrittura sono sincrone verso l'HNSW in-memory;
//! la persistenza su DB è asincrona (fire-and-forget) per non bloccare il
//! critical path.
//!
//! # Ciclo di vita tipico
//! ```text
//!   let store = RuVectorStore::new("agents")
//!       .with_pool(pool);
//!
//!   // al boot: ricostruisce l'HNSW da DB (background)
//!   store.load_from_db().await?;
//!
//!   // durante il run: insert persist automaticamente
//!   store.insert_with_persist("agent:Coder", vec![...], None, 1.0);
//!
//!   // search: solo in-memory, sub-millisecondo
//!   let results = store.search(&query, 5)?;
//! ```

use crate::core::HnswDb;
use crate::persistence;
use crate::types::{HnswConfig, HnswStats, InsertOptions, Result, SearchResult, VectorMetadata};
use serde_json::Value as Json;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

// ─── RuVectorStore ────────────────────────────────────────────────────────────

/// Store per una singola collection vettoriale.
///
/// Thread-safe: può essere messo in `Arc<RuVectorStore>` e condiviso tra tokio task.
pub struct RuVectorStore {
    /// Nome della collection (es. "agents", "tasks", "patterns", "memory")
    pub collection: String,
    /// Indice HNSW in-memory
    db: HnswDb,
    /// Pool opzionale per persistenza PostgreSQL
    pool: Option<Arc<PgPool>>,
}

impl RuVectorStore {
    /// Crea uno store in-memory puro, senza persistenza.
    pub fn new(collection: impl Into<String>) -> Self {
        Self::with_config(collection, HnswConfig::default())
    }

    /// Crea con configurazione HNSW personalizzata.
    pub fn with_config(collection: impl Into<String>, config: HnswConfig) -> Self {
        Self {
            collection: collection.into(),
            db: HnswDb::new(config),
            pool: None,
        }
    }

    /// Aggiunge un pool PostgreSQL per la persistenza.
    pub fn with_pool(mut self, pool: Arc<PgPool>) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Ritorna `true` se la persistenza è configurata.
    pub fn has_persistence(&self) -> bool {
        self.pool.is_some()
    }

    // ─── Boot loading ────────────────────────────────────────────────────────

    /// Carica i vettori dal database e li inserisce nell'HNSW in-memory.
    ///
    /// Da chiamare una volta sola al boot, preferibilmente in un tokio task.
    /// Ritorna il numero di vettori caricati.
    ///
    /// Se il pool non è configurato, ritorna Ok(0) silenziosamente.
    pub async fn load_from_db(&self) -> anyhow::Result<usize> {
        let pool = match &self.pool {
            Some(p) => p.clone(),
            None => {
                debug!("RuVectorStore[{}]: no pool, skip load_from_db", self.collection);
                return Ok(0);
            }
        };

        let vectors = persistence::load_collection_vectors(&pool, &self.collection).await?;
        let total = vectors.len();

        if total == 0 {
            info!("RuVectorStore[{}]: collection vuota — nessun vettore da caricare", self.collection);
            return Ok(0);
        }

        let mut loaded = 0usize;
        let mut errors = 0usize;

        for pv in vectors {
            let meta = VectorMetadata {
                id: pv.external_id.clone(),
                namespace: self.collection.clone(),
                tags: extract_tags(&pv.metadata),
                created_at: chrono::Utc::now(),
                ttl_seconds: None,
            };

            match self.db.insert(pv.external_id.clone(), pv.embedding, Some(meta)) {
                Ok(_) => loaded += 1,
                Err(e) => {
                    warn!(
                        "RuVectorStore[{}]: errore caricamento '{}': {}",
                        self.collection, pv.external_id, e
                    );
                    errors += 1;
                }
            }
        }

        info!(
            "RuVectorStore[{}]: caricati {}/{} vettori ({} errori)",
            self.collection, loaded, total, errors
        );

        Ok(loaded)
    }

    // ─── Insert ──────────────────────────────────────────────────────────────

    /// Inserisce in-memory (solo HNSW, no DB).
    pub fn insert(
        &self,
        id: impl Into<String>,
        vector: Vec<f32>,
        metadata: Option<VectorMetadata>,
    ) -> Result<usize> {
        self.db.insert(id.into(), vector, metadata)
    }

    /// Inserisce in-memory **e** persiste in DB (async fire-and-forget).
    ///
    /// La scrittura su DB non blocca: viene spawnata come tokio task.
    /// Se il pool non è configurato, equivale a `insert`.
    pub fn insert_with_persist(
        &self,
        id: impl Into<String>,
        vector: Vec<f32>,
        metadata: Option<VectorMetadata>,
        confidence: f32,
    ) -> Result<usize> {
        let id_str: String = id.into();

        // 1. Insert HNSW (sincrono, fast path) — propaga confidence per SONA pruning
        let node_id = self.db.insert_with_confidence(id_str.clone(), vector.clone(), metadata.clone(), confidence)?;

        // 2. Persist a DB (asincrono)
        if let Some(pool) = &self.pool {
            let pool_clone = pool.clone();
            let collection = self.collection.clone();
            let meta_json = metadata
                .as_ref()
                .and_then(|m| serde_json::to_value(m).ok())
                .unwrap_or(serde_json::json!({}));

            tokio::spawn(async move {
                if let Err(e) = persistence::upsert_vector(
                    &pool_clone,
                    &collection,
                    &id_str,
                    &vector,
                    &meta_json,
                    confidence,
                )
                .await
                {
                    warn!("RuVectorStore[{}]: persist failed for '{}': {}", collection, id_str, e);
                }
            });
        }

        Ok(node_id)
    }

    /// Inserisce un batch di vettori con persistenza.
    /// Ritorna (inserted, failed).
    pub fn insert_batch_with_persist(
        &self,
        items: Vec<(String, Vec<f32>, Option<VectorMetadata>, f32)>,
    ) -> (usize, usize) {
        let mut inserted = 0usize;
        let mut failed = 0usize;

        for (id, vector, metadata, confidence) in items {
            match self.insert_with_persist(id, vector, metadata, confidence) {
                Ok(_) => inserted += 1,
                Err(_) => failed += 1,
            }
        }

        (inserted, failed)
    }

    // ─── Search ──────────────────────────────────────────────────────────────

    /// Ricerca i k vettori più vicini (solo HNSW, sub-millisecondale).
    pub fn search(&self, query_vector: &[f32], k: usize) -> Result<Vec<SearchResult>> {
        self.db.search(query_vector, k)
    }

    // ─── Stats ───────────────────────────────────────────────────────────────

    /// Statistiche correnti dell'indice HNSW.
    pub fn stats(&self) -> HnswStats {
        self.db.stats()
    }

    /// Conta i vettori nel DB (non in-memory).
    /// Utile per verificare la consistenza al boot.
    pub async fn db_count(&self) -> anyhow::Result<i64> {
        let pool = match &self.pool {
            Some(p) => p.clone(),
            None => return Ok(0),
        };

        let row = sqlx::query(
            r#"
            SELECT COUNT(*) as n
            FROM   ruvector_vectors rv
            JOIN   ruvector_collections rc ON rv.collection_id = rc.id
            WHERE  rc.name = $1 AND rv.deleted = FALSE
            "#,
        )
        .bind(&self.collection)
        .fetch_one(pool.as_ref())
        .await?;

        use sqlx::Row;
        Ok(row.try_get::<i64, _>("n").unwrap_or(0))
    }

    /// Soft-delete di un vettore da DB (in-memory non viene rimosso,
    /// verrà escluso al prossimo boot).
    pub async fn delete(&self, external_id: &str) -> anyhow::Result<bool> {
        let pool = match &self.pool {
            Some(p) => p.clone(),
            None => return Ok(false),
        };
        persistence::soft_delete_vector(&pool, &self.collection, external_id).await
    }

    /// Converte InsertOptions in metadati per l'insert.
    pub fn meta_from_opts(&self, id: &str, opts: &InsertOptions) -> VectorMetadata {
        VectorMetadata {
            id: id.to_string(),
            namespace: opts.namespace.clone(),
            tags: opts.tags.clone(),
            created_at: chrono::Utc::now(),
            ttl_seconds: opts.ttl_seconds,
        }
    }
}

// ─── RuVectorManager ─────────────────────────────────────────────────────────

/// Manager globale di più store.
/// Usato in `NexusBridge` per gestire le 4 collection predefinite.
pub struct RuVectorManager {
    stores: dashmap::DashMap<String, Arc<RuVectorStore>>,
}

impl RuVectorManager {
    /// Le 4 collection predefinite della migration 0052.
    pub const DEFAULT_COLLECTIONS: &'static [&'static str] =
        &["agents", "tasks", "patterns", "memory"];

    /// Crea il manager inizializzando le 4 collection di default.
    pub fn new(pool: Arc<PgPool>) -> Self {
        let stores = dashmap::DashMap::new();
        for &name in Self::DEFAULT_COLLECTIONS {
            let store = Arc::new(
                RuVectorStore::new(name).with_pool(pool.clone()),
            );
            stores.insert(name.to_string(), store);
        }
        Self { stores }
    }

    /// Accede a uno store per nome.
    pub fn get(&self, collection: &str) -> Option<Arc<RuVectorStore>> {
        self.stores.get(collection).map(|e| e.value().clone())
    }

    /// Carica tutte le collection dal DB, in parallelo.
    /// Ritorna una mappa collection_name → vettori_caricati.
    pub async fn load_all_from_db(&self) -> Vec<(String, usize)> {
        let mut handles = Vec::new();

        for entry in self.stores.iter() {
            let store = entry.value().clone();
            let name = entry.key().clone();
            handles.push(tokio::spawn(async move {
                match store.load_from_db().await {
                    Ok(n) => (name, n),
                    Err(e) => {
                        warn!("RuVectorManager: load_from_db failed for '{}': {}", name, e);
                        (name, 0)
                    }
                }
            }));
        }

        let mut results = Vec::new();
        for h in handles {
            if let Ok(r) = h.await {
                results.push(r);
            }
        }
        results
    }

    /// Numero di vettori totali in-memory su tutte le collection.
    pub fn total_in_memory(&self) -> usize {
        self.stores
            .iter()
            .map(|e| e.value().stats().total_nodes)
            .sum()
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Estrae tags da metadati JSON (oggetto piatto key→string).
fn extract_tags(metadata: &Json) -> HashMap<String, String> {
    metadata
        .as_object()
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store() -> RuVectorStore {
        RuVectorStore::new("test_collection")
    }

    #[test]
    fn test_store_insert_and_search() {
        let store = make_store();

        store.insert("v1", vec![1.0, 0.0, 0.0], None).unwrap();
        store.insert("v2", vec![0.0, 1.0, 0.0], None).unwrap();
        store.insert("v3", vec![0.9, 0.1, 0.0], None).unwrap();

        let results = store.search(&[1.0, 0.0, 0.0], 2).unwrap();
        assert_eq!(results.len(), 2);
        // Il primo risultato deve essere più vicino
        assert!(results[0].distance <= results[1].distance);
    }

    #[test]
    fn test_store_search_empty_returns_empty() {
        let store = make_store();
        let results = store.search(&[1.0, 0.0], 5).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_store_stats_after_inserts() {
        let store = make_store();
        store.insert("a", vec![1.0, 2.0], None).unwrap();
        store.insert("b", vec![3.0, 4.0], None).unwrap();

        let stats = store.stats();
        assert_eq!(stats.total_nodes, 2);
        assert!(stats.entry_point.is_some());
    }

    #[test]
    fn test_store_has_no_persistence_without_pool() {
        let store = make_store();
        assert!(!store.has_persistence());
    }

    #[test]
    fn test_extract_tags_flat_object() {
        let meta = serde_json::json!({ "type": "agent", "role": "coder" });
        let tags = extract_tags(&meta);
        assert_eq!(tags.get("type").map(|s| s.as_str()), Some("agent"));
        assert_eq!(tags.get("role").map(|s| s.as_str()), Some("coder"));
    }

    #[test]
    fn test_extract_tags_ignores_non_string() {
        let meta = serde_json::json!({ "count": 42, "name": "foo" });
        let tags = extract_tags(&meta);
        assert!(!tags.contains_key("count")); // numeric → ignorato
        assert_eq!(tags.get("name").map(|s| s.as_str()), Some("foo"));
    }

    #[tokio::test]
    async fn test_load_from_db_without_pool_returns_zero() {
        let store = make_store(); // no pool
        let n = store.load_from_db().await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn test_db_count_without_pool_returns_zero() {
        let store = make_store();
        assert_eq!(store.db_count().await.unwrap(), 0);
    }

    #[test]
    fn test_manager_has_four_default_collections() {
        // RuVectorManager::new richiede un PgPool reale.
        // Verifichiamo solo i costanti.
        assert_eq!(RuVectorManager::DEFAULT_COLLECTIONS.len(), 4);
        assert!(RuVectorManager::DEFAULT_COLLECTIONS.contains(&"agents"));
        assert!(RuVectorManager::DEFAULT_COLLECTIONS.contains(&"tasks"));
        assert!(RuVectorManager::DEFAULT_COLLECTIONS.contains(&"patterns"));
        assert!(RuVectorManager::DEFAULT_COLLECTIONS.contains(&"memory"));
    }

    #[test]
    fn test_batch_insert() {
        let store = make_store();
        let items = vec![
            ("x1".to_string(), vec![1.0f32, 0.0, 0.0], None, 1.0f32),
            ("x2".to_string(), vec![0.0f32, 1.0, 0.0], None, 0.9f32),
            ("x3".to_string(), vec![0.5f32, 0.5, 0.0], None, 0.8f32),
        ];
        let (inserted, failed) = store.insert_batch_with_persist(items);
        assert_eq!(inserted, 3);
        assert_eq!(failed, 0);
        assert_eq!(store.stats().total_nodes, 3);
    }
}
