use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Vector metadata - informazioni associate a ogni vettore
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VectorMetadata {
    pub id: String,
    pub namespace: String,
    pub tags: HashMap<String, String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub ttl_seconds: Option<u32>,
}

/// Risultato di una ricerca nel vettore
#[derive(Clone, Debug)]
pub struct SearchResult {
    pub id: String,
    pub score: f32,
    pub distance: f32,
    pub metadata: Option<VectorMetadata>,
}

/// Errori RuVector
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Vector not found: {0}")]
    NotFound(String),

    #[error("Invalid vector dimension: expected {expected}, got {actual}")]
    InvalidDimension { expected: usize, actual: usize },

    #[error("Database error: {0}")]
    DatabaseError(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("HNSW error: {0}")]
    HnswError(String),

    #[error("{0}")]
    Other(String),
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error::SerializationError(err.to_string())
    }
}

impl From<anyhow::Error> for Error {
    fn from(err: anyhow::Error) -> Self {
        Error::Other(err.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Configurazione HNSW
#[derive(Clone, Debug)]
pub struct HnswConfig {
    /// Numero massimo di connessioni per nodo
    pub m_max: usize,
    /// Fattore moltiplicativo per M (di solito 2)
    pub m_l: f32,
    /// Numero di neighbor da valutare durante costruzione
    pub ef_construction: usize,
    /// Numero di neighbor da valutare durante search
    pub ef_search: usize,
    /// Seed per randomizzazione
    pub seed: u64,
}

impl Default for HnswConfig {
    fn default() -> Self {
        Self {
            m_max: 16,
            m_l: 1.0 / (2.0_f32.ln()),
            ef_construction: 200,
            ef_search: 100,
            seed: 42,
        }
    }
}

/// Nodo HNSW
#[derive(Clone, Debug)]
pub struct HnswNode {
    /// Indice interno nella Vec (node_id)
    pub id: usize,
    /// Id esterno passato a `insert()` — sempre presente
    pub external_id: String,
    pub level: usize,
    pub neighbors: Vec<Vec<usize>>, // neighbors[level] = list of neighbor ids
    pub vector: Vec<f32>,
    pub metadata: Option<VectorMetadata>,
    /// Soft-delete: nodo escluso dalla ricerca senza rimuoverlo dalla Vec
    pub deleted: bool,
    /// Confidenza (0.0–1.0) usata da SONA pruning. 1.0 = piena confidenza.
    pub confidence: f32,
}

/// Configurazione inserimento vettore
#[derive(Clone, Debug)]
pub struct InsertOptions {
    pub namespace: String,
    pub tags: HashMap<String, String>,
    pub ttl_seconds: Option<u32>,
}

impl Default for InsertOptions {
    fn default() -> Self {
        Self {
            namespace: "default".to_string(),
            tags: HashMap::new(),
            ttl_seconds: None,
        }
    }
}

/// Risultato operazione batch insert
#[derive(Clone, Debug)]
pub struct BatchInsertResult {
    pub inserted: usize,
    pub failed: usize,
    pub ids: Vec<String>,
}

/// Statistiche HNSW esposte da `HnswDb::stats()`
#[derive(Debug, Clone)]
pub struct HnswStats {
    pub total_nodes: usize,
    pub avg_neighbors: usize,
    pub entry_point: Option<usize>,
}
