//! RuVector - High-performance vector database with HNSW
//!
//! A Rust implementation of hierarchical navigable small world (HNSW)
//! for efficient similarity search in high-dimensional spaces.
//!
//! # Modules
//! - `core`        — HNSW algorithm (insert, search, stats)
//! - `types`       — tipi pubblici (HnswConfig, VectorMetadata, SearchResult, ...)
//! - `persistence` — persistenza PostgreSQL (upsert_vector, load_collection_vectors)
//! - `store`       — `RuVectorStore` + `RuVectorManager` (API ad alto livello)

pub mod core;
pub mod persistence;
pub mod store;
pub mod types;

pub use core::{cosine_distance, euclidean_distance, HnswDb, Metric, OptimizeStats};
pub use types::HnswStats;
pub use persistence::{
    get_collection_id, load_collection_vectors, record_hnsw_stats, soft_delete_vector,
    upsert_vector, PersistedVector,
};
pub use store::{RuVectorManager, RuVectorStore};
pub use types::{
    BatchInsertResult, Error, HnswConfig, InsertOptions, Result, SearchResult, VectorMetadata,
};

// Re-export per retrocompatibilità
pub use core::HnswDb as Database;
