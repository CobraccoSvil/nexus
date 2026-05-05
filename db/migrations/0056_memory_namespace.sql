-- Migration 0056: Memory Namespace — memoria condivisa tra agenti (CRDT-friendly).
--
-- Il MemoryNamespace è la "memoria di lavoro" condivisa tra gli agenti
-- durante l'esecuzione di task paralleli in un SwarmCoordinator.
--
-- Progettazione CRDT (Conflict-free Replicated Data Type):
--   - Ogni write porta un vector clock / timestamp
--   - I conflitti si risolvono con LWW (Last-Write-Wins) per default
--   - Supporto per merge semantico (configurabile per namespace)
--
-- Tabelle:
--   memory_namespaces     → definizione namespace (isolamento per progetto/swarm)
--   memory_entries        → coppie key-value con versioning
--   memory_snapshots      → snapshot periodici (per recovery)
--   memory_subscriptions  → pub/sub registrations per swarm coordination

-- ---------------------------------------------------------------------------
-- Tabella: memory_namespaces
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS memory_namespaces (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Identificatore unico del namespace
    ns_key          TEXT        NOT NULL UNIQUE,
    -- Tipo di namespace
    ns_type         TEXT        NOT NULL DEFAULT 'swarm'
                    CHECK (ns_type IN ('swarm', 'project', 'agent', 'global')),
    -- Riferimento opzionale a progetto
    project_id      UUID,
    -- TTL in secondi (NULL = permanente)
    ttl_seconds     INTEGER,
    -- Conflict resolution strategy
    merge_strategy  TEXT        NOT NULL DEFAULT 'lww'
                    CHECK (merge_strategy IN ('lww', 'semantic', 'manual')),
    -- Numero massimo di entries
    max_entries     INTEGER     NOT NULL DEFAULT 10000,
    -- Stato
    active          BOOLEAN     NOT NULL DEFAULT TRUE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at      TIMESTAMPTZ,   -- NOW() + ttl_seconds (computato all'insert)
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE memory_namespaces IS
    'Namespace di memoria condivisa tra agenti in un swarm.';

CREATE INDEX IF NOT EXISTS idx_memory_namespaces_ns_key
    ON memory_namespaces (ns_key)
    WHERE active = TRUE;

CREATE INDEX IF NOT EXISTS idx_memory_namespaces_project
    ON memory_namespaces (project_id)
    WHERE project_id IS NOT NULL AND active = TRUE;

-- ---------------------------------------------------------------------------
-- Tabella: memory_entries
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS memory_entries (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    namespace_id    UUID        NOT NULL REFERENCES memory_namespaces(id) ON DELETE CASCADE,
    -- Chiave semantica (es. "task:result", "code:snippet", "decision:arch")
    entry_key       TEXT        NOT NULL,
    -- Valore (JSON arbitrario)
    value           JSONB       NOT NULL,
    -- Vettore semantico del valore (per similarity search nel namespace)
    embedding       float4[],
    -- Versioning CRDT
    version         BIGINT      NOT NULL DEFAULT 1,
    vector_clock    JSONB       NOT NULL DEFAULT '{}',  -- {agent_id: counter}
    -- Autore (agent type che ha scritto)
    written_by      TEXT,
    -- Soft-delete
    deleted         BOOLEAN     NOT NULL DEFAULT FALSE,
    -- TTL opzionale (per entries temporanee)
    expires_at      TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE memory_entries IS
    'Coppie key-value con versioning CRDT per memoria condivisa agenti.';

-- Lookup principale: (namespace, key) → valore corrente
CREATE UNIQUE INDEX IF NOT EXISTS idx_memory_entries_unique_key
    ON memory_entries (namespace_id, entry_key)
    WHERE deleted = FALSE;

-- Indice temporale per sync e TTL cleanup
CREATE INDEX IF NOT EXISTS idx_memory_entries_updated_at
    ON memory_entries (namespace_id, updated_at DESC)
    WHERE deleted = FALSE;

-- Indice per similarity search (non GIN — float4[] richiede pg_vector o custom)
-- Qui usiamo un indice su namespace per filtrare prima del search in Rust
CREATE INDEX IF NOT EXISTS idx_memory_entries_namespace
    ON memory_entries (namespace_id)
    WHERE deleted = FALSE AND embedding IS NOT NULL;

-- ---------------------------------------------------------------------------
-- Tabella: memory_snapshots
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS memory_snapshots (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    namespace_id    UUID        NOT NULL REFERENCES memory_namespaces(id) ON DELETE CASCADE,
    -- Snapshot completo (JSON compresso del namespace)
    snapshot_data   JSONB       NOT NULL,
    -- Numero di entries al momento dello snapshot
    entry_count     INTEGER     NOT NULL DEFAULT 0,
    -- Versione massima inclusa
    max_version     BIGINT      NOT NULL DEFAULT 0,
    -- Motivo dello snapshot
    reason          TEXT        NOT NULL DEFAULT 'periodic'
                    CHECK (reason IN ('periodic', 'pre_merge', 'recovery', 'shutdown')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_memory_snapshots_namespace
    ON memory_snapshots (namespace_id, created_at DESC);

-- ---------------------------------------------------------------------------
-- Tabella: memory_subscriptions
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS memory_subscriptions (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    namespace_id    UUID        NOT NULL REFERENCES memory_namespaces(id) ON DELETE CASCADE,
    -- Agente sottoscritto
    subscriber_id   TEXT        NOT NULL,
    agent_type      TEXT,
    -- Pattern di chiavi di interesse (NULL = tutte)
    key_pattern     TEXT,
    -- Stato
    active          BOOLEAN     NOT NULL DEFAULT TRUE,
    last_seen_at    TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (namespace_id, subscriber_id)
);

CREATE INDEX IF NOT EXISTS idx_memory_subscriptions_namespace
    ON memory_subscriptions (namespace_id)
    WHERE active = TRUE;

-- ---------------------------------------------------------------------------
-- Helper: cleanup expired entries (da chiamare periodicamente)
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION cleanup_expired_memory_entries()
RETURNS INTEGER AS $$
DECLARE
    deleted_count INTEGER;
BEGIN
    UPDATE memory_entries
    SET deleted = TRUE, updated_at = NOW()
    WHERE expires_at IS NOT NULL
      AND expires_at < NOW()
      AND deleted = FALSE;
    GET DIAGNOSTICS deleted_count = ROW_COUNT;
    RETURN deleted_count;
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION cleanup_expired_memory_entries() IS
    'Soft-delete entries scadute. Da invocare da un cron job o background worker.';

-- ---------------------------------------------------------------------------
-- Namespace predefiniti
-- ---------------------------------------------------------------------------
INSERT INTO memory_namespaces (ns_key, ns_type, merge_strategy, max_entries)
VALUES
    ('global',      'global',  'lww',      100000),
    ('agents',      'agent',   'lww',       10000),
    ('reasoning',   'global',  'semantic',  50000)
ON CONFLICT (ns_key) DO NOTHING;
