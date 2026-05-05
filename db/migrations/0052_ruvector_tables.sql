-- Migration 0052: RuVector — database vettoriale nativo per Nexus.
--
-- Persistenza dei vettori semantici usati da:
--   - Q-Learning router (embedding agenti + task)
--   - MemoryNamespace (memoria condivisa tra agenti)
--   - LearningWorkers (pattern storage)
--
-- Struttura:
--   ruvector_collections  → namespace logici (per agente, progetto, tipo)
--   ruvector_vectors      → vettori float[] + metadati JSON
--   ruvector_hnsw_stats   → statistiche HNSW per monitoring
--
-- Nota: i vettori sono storati come float4[] (32-bit) per efficienza.
-- La dimensione default è 384 (MiniLM-L6-v2). Dimensioni diverse
-- sono supportate per collection.

-- ---------------------------------------------------------------------------
-- Tabella: collections
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS ruvector_collections (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    name            TEXT        NOT NULL UNIQUE,
    description     TEXT,
    dim             INTEGER     NOT NULL DEFAULT 384,
    -- Politiche di retention
    max_vectors     INTEGER,                     -- NULL = unlimited
    ttl_seconds     INTEGER,                     -- NULL = no expiry
    -- Metadati
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE ruvector_collections IS
    'Namespace logici per i vettori RuVector. Ogni collection è isolata.';

-- Indice su name (già UNIQUE, ma utile per LIKE queries)
CREATE INDEX IF NOT EXISTS idx_ruvector_collections_name
    ON ruvector_collections (name);

-- ---------------------------------------------------------------------------
-- Tabella: vectors
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS ruvector_vectors (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    collection_id   UUID        NOT NULL REFERENCES ruvector_collections(id) ON DELETE CASCADE,
    -- Identificatore semantico (es. "agent:Coder", "pattern:xyz")
    external_id     TEXT        NOT NULL,
    -- Vettore float32 — dimensione = collection.dim
    embedding       float4[]    NOT NULL,
    -- Metadati arbitrari (JSON)
    metadata        JSONB       NOT NULL DEFAULT '{}',
    -- Soft-delete per evitare re-index frequenti
    deleted         BOOLEAN     NOT NULL DEFAULT FALSE,
    -- Confidenza (0-1), usata da SONA per pruning
    confidence      REAL        NOT NULL DEFAULT 1.0,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE ruvector_vectors IS
    'Vettori semantici persistiti. embedding è float4[] di dim dimensioni.';

-- Indice principale per similarity search per collection
CREATE INDEX IF NOT EXISTS idx_ruvector_vectors_collection
    ON ruvector_vectors (collection_id)
    WHERE deleted = FALSE;

-- Indice su external_id per lookup diretto
CREATE INDEX IF NOT EXISTS idx_ruvector_vectors_external_id
    ON ruvector_vectors (collection_id, external_id)
    WHERE deleted = FALSE;

-- Indice temporale per TTL cleanup
CREATE INDEX IF NOT EXISTS idx_ruvector_vectors_created_at
    ON ruvector_vectors (collection_id, created_at DESC)
    WHERE deleted = FALSE;

-- UNIQUE per evitare duplicati (upsert)
CREATE UNIQUE INDEX IF NOT EXISTS idx_ruvector_vectors_unique_external
    ON ruvector_vectors (collection_id, external_id)
    WHERE deleted = FALSE;

-- ---------------------------------------------------------------------------
-- Tabella: hnsw_stats
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS ruvector_hnsw_stats (
    id              BIGSERIAL   PRIMARY KEY,
    collection_id   UUID        NOT NULL REFERENCES ruvector_collections(id) ON DELETE CASCADE,
    -- Snapshot HNSW metrics
    num_vectors     INTEGER     NOT NULL DEFAULT 0,
    num_layers      INTEGER     NOT NULL DEFAULT 0,
    avg_connections REAL        NOT NULL DEFAULT 0.0,
    -- Performance
    last_insert_us  BIGINT,     -- microseconds
    last_search_us  BIGINT,
    last_optimize_us BIGINT,
    -- SONA
    sona_runs       INTEGER     NOT NULL DEFAULT 0,
    sona_pruned     INTEGER     NOT NULL DEFAULT 0,
    recorded_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_ruvector_hnsw_stats_collection
    ON ruvector_hnsw_stats (collection_id, recorded_at DESC);

-- ---------------------------------------------------------------------------
-- Collezioni predefinite
-- ---------------------------------------------------------------------------
INSERT INTO ruvector_collections (name, description, dim, max_vectors)
VALUES
    ('agents',     'Profili embedding degli agent types',          384, 200),
    ('tasks',      'Embedding dei task per routing storico',       384, 10000),
    ('patterns',   'Pattern estratti da LearningWorkers',          384, 50000),
    ('memory',     'MemoryNamespace — memoria condivisa agenti',   384, NULL)
ON CONFLICT (name) DO NOTHING;
