//! DTO condivisi per le ricerche vettoriali Qdrant (regola L / ADR 0026).
//!
//! Estratto da mcp-core::vector_memory (split 7.4): usato sia dalle ricerche
//! generali del monolite sia dalla famiglia wiki content points in nexus-wiki.

use serde_json::Value;

/// Hit di una ricerca vettoriale: id del punto, score di similarita', payload.
#[derive(Debug, Clone)]
pub struct VectorPointHit {
    pub point_id: String,
    pub score: f64,
    pub payload: Value,
}
