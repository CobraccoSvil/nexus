-- Tabella per persistenza dei batch di replicazione prodotti da ReplicationWorker.
--
-- Il worker (ReplicationWorker, periodic ogni 3 min) prepara un batch JSON
-- nel namespace di observability sotto la chiave "replication:pending".
-- Il consumer (NexusBridge::flush_replication_pending) legge il batch e
-- scrive ogni entry qui, via UPSERT su (namespace_id, key).
--
-- Chiamato anche durante il graceful shutdown per garantire zero perdita dati.

CREATE TABLE IF NOT EXISTS nexus_replication_log (
    id              BIGSERIAL       PRIMARY KEY,
    namespace_id    TEXT            NOT NULL,
    key             TEXT            NOT NULL,
    value           JSONB           NOT NULL DEFAULT '{}',
    author          TEXT            NOT NULL DEFAULT '',
    replicated_at   TIMESTAMPTZ     NOT NULL DEFAULT NOW(),

    UNIQUE (namespace_id, key)
);

-- Index per query per namespace (es. "seleziona tutte le entry di un namespace")
CREATE INDEX IF NOT EXISTS idx_nexus_replication_log_namespace
    ON nexus_replication_log (namespace_id);

-- Index per pulizia TTL (elimina entry più vecchie di N giorni)
CREATE INDEX IF NOT EXISTS idx_nexus_replication_log_replicated_at
    ON nexus_replication_log (replicated_at DESC);

COMMENT ON TABLE nexus_replication_log IS
    'Copia persistente delle entry namespace prodotte da ReplicationWorker. '
    'Aggiornata ogni ~3 minuti da NexusBridge::flush_replication_pending(). '
    'Ogni riga è identificata da (namespace_id, key): un UPSERT aggiorna il valore.';

COMMENT ON COLUMN nexus_replication_log.namespace_id IS
    'Identificatore del namespace sorgente (es. "nexus-bridge-global").';

COMMENT ON COLUMN nexus_replication_log.key IS
    'Chiave dell''entry namespace (es. "pattern:abc", "metrics:latest", "version:20260415").';

COMMENT ON COLUMN nexus_replication_log.value IS
    'Valore JSON dell''entry (arbitrario, dipende dal worker che l''ha prodotta).';

COMMENT ON COLUMN nexus_replication_log.author IS
    'Worker o componente che ha scritto l''entry originale nel namespace.';
