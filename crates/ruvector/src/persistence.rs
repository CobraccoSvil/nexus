//! Persistence layer PostgreSQL per RuVector.
//!
//! Tutte le funzioni sono stateless e ricevono il pool come parametro,
//! così possono essere usate sia da `RuVectorStore` che direttamente.
//!
//! Schema di riferimento (migration 0052):
//! - `ruvector_collections` (id UUID, name TEXT UNIQUE, dim INTEGER, ...)
//! - `ruvector_vectors`    (id UUID, collection_id UUID FK, external_id TEXT,
//!                          embedding float4[], metadata JSONB, deleted BOOL,
//!                          confidence REAL, created_at, updated_at)
//!
//! Nota sul tipo `float4[]`: sqlx mappa `Vec<f32>` ↔ `float4[]` con la
//! feature `postgres`. Non serve alcuna serializzazione custom.

use serde_json::Value as Json;
use sqlx::PgPool;

/// Un vettore caricato dal database.
#[derive(Clone, Debug)]
pub struct PersistedVector {
    /// Identificatore semantico (es. "agent:Coder", "task:abc123")
    pub external_id: String,
    /// Float32 embedding
    pub embedding: Vec<f32>,
    /// Metadati JSON opzionali
    pub metadata: Json,
    /// Confidenza (0-1). Usata per pruning SONA.
    pub confidence: f32,
}

// ─── Helpers interni ─────────────────────────────────────────────────────────

/// Ritorna l'UUID della collection dato il nome, o `None` se non esiste.
pub async fn get_collection_id(
    pool: &PgPool,
    collection_name: &str,
) -> anyhow::Result<Option<uuid::Uuid>> {
    let row = sqlx::query(
        "SELECT id FROM ruvector_collections WHERE name = $1",
    )
    .bind(collection_name)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| {
        use sqlx::Row;
        r.get::<uuid::Uuid, _>("id")
    }))
}

// ─── API pubblica ─────────────────────────────────────────────────────────────

/// Carica tutti i vettori non cancellati di una collection.
///
/// Ritorna un `Vec<PersistedVector>` ordinato per `created_at ASC`
/// (ordine stabile per la ricostruzione dell'indice HNSW).
pub async fn load_collection_vectors(
    pool: &PgPool,
    collection_name: &str,
) -> anyhow::Result<Vec<PersistedVector>> {
    let rows = sqlx::query(
        r#"
        SELECT rv.external_id,
               rv.embedding,
               rv.metadata,
               rv.confidence
        FROM   ruvector_vectors rv
        JOIN   ruvector_collections rc ON rv.collection_id = rc.id
        WHERE  rc.name   = $1
          AND  rv.deleted = FALSE
        ORDER BY rv.created_at ASC
        "#,
    )
    .bind(collection_name)
    .fetch_all(pool)
    .await?;

    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        use sqlx::Row;
        let external_id: String       = row.try_get("external_id")?;
        let embedding: Vec<f32>       = row.try_get("embedding")?;
        let metadata: Json            = row.try_get("metadata")?;
        let confidence: f32           = row.try_get("confidence")?;
        result.push(PersistedVector { external_id, embedding, metadata, confidence });
    }
    Ok(result)
}

/// Inserisce o aggiorna un vettore nella collection (upsert su `external_id`).
///
/// Se `external_id` esiste già (non cancellato) → aggiorna embedding, metadata
/// e confidence. Altrimenti inserisce una nuova riga.
///
/// La collection deve esistere già (`ruvector_collections` deve avere la riga).
/// Se la collection non esiste, ritorna errore.
pub async fn upsert_vector(
    pool: &PgPool,
    collection_name: &str,
    external_id: &str,
    embedding: &[f32],
    metadata: &Json,
    confidence: f32,
) -> anyhow::Result<()> {
    let affected = sqlx::query(
        r#"
        WITH coll AS (
            SELECT id FROM ruvector_collections WHERE name = $1
        )
        INSERT INTO ruvector_vectors
            (collection_id, external_id, embedding, metadata, confidence)
        SELECT coll.id, $2, $3::float4[], $4::jsonb, $5
        FROM   coll
        ON CONFLICT (collection_id, external_id)
            WHERE deleted = FALSE
        DO UPDATE SET
            embedding   = EXCLUDED.embedding,
            metadata    = EXCLUDED.metadata,
            confidence  = EXCLUDED.confidence,
            updated_at  = NOW()
        "#,
    )
    .bind(collection_name)
    .bind(external_id)
    .bind(embedding)
    .bind(metadata)
    .bind(confidence)
    .execute(pool)
    .await?
    .rows_affected();

    if affected == 0 {
        anyhow::bail!(
            "ruvector upsert: collection '{}' non trovata",
            collection_name
        );
    }
    Ok(())
}

/// Soft-delete di un vettore (setta `deleted = TRUE`).
/// Ritorna `true` se la riga è stata trovata e cancellata, `false` altrimenti.
pub async fn soft_delete_vector(
    pool: &PgPool,
    collection_name: &str,
    external_id: &str,
) -> anyhow::Result<bool> {
    let affected = sqlx::query(
        r#"
        UPDATE ruvector_vectors rv
        SET    deleted    = TRUE,
               updated_at = NOW()
        FROM   ruvector_collections rc
        WHERE  rc.name         = $1
          AND  rv.collection_id = rc.id
          AND  rv.external_id  = $2
          AND  rv.deleted      = FALSE
        "#,
    )
    .bind(collection_name)
    .bind(external_id)
    .execute(pool)
    .await?
    .rows_affected();

    Ok(affected > 0)
}

/// Salva uno snapshot delle statistiche HNSW per monitoring.
pub async fn record_hnsw_stats(
    pool: &PgPool,
    collection_name: &str,
    num_vectors: i32,
    num_layers: i32,
    avg_connections: f32,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO ruvector_hnsw_stats
            (collection_id, num_vectors, num_layers, avg_connections)
        SELECT id, $2, $3, $4
        FROM   ruvector_collections WHERE name = $1
        "#,
    )
    .bind(collection_name)
    .bind(num_vectors)
    .bind(num_layers)
    .bind(avg_connections)
    .execute(pool)
    .await?;
    Ok(())
}

// ─── Tests ─────────────────────────────────────────────────────────────────--

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializzazione round-trip del PersistedVector (senza DB reale).
    #[test]
    fn test_persisted_vector_clone() {
        let v = PersistedVector {
            external_id: "agent:Coder".to_string(),
            embedding: vec![0.1, 0.2, 0.3],
            metadata: serde_json::json!({ "type": "agent" }),
            confidence: 0.95,
        };
        let v2 = v.clone();
        assert_eq!(v2.external_id, "agent:Coder");
        assert!((v2.confidence - 0.95).abs() < 1e-6);
    }
}
