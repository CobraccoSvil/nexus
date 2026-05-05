-- Storico delle migrazioni applicate al DB di ogni progetto utente.
-- Ogni file di migration creato da un agente (o dall'utente) viene
-- registrato qui con checksum, stato, audit trail.
--
-- Stati:
--   pending          migration creata ma non ancora applicata
--   pending_override DDL raw in attesa di conferma UI dell'utente
--   applied          migration applicata con successo
--   rolled_back      migration annullata con project_db_rollback
--   overridden       DDL raw eseguito dopo conferma override
--   failed           applicazione fallita (vedi error_message)

CREATE TABLE IF NOT EXISTS project_migration_history (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,

    -- Nome del file migration (es. "20260422_120000_add_users_table.sql").
    filename TEXT NOT NULL,

    -- SHA-256 del contenuto del file, per rilevare modifiche successive.
    checksum TEXT NOT NULL,

    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'pending_override', 'applied', 'rolled_back', 'overridden', 'failed')),

    -- Descrizione human-readable fornita dall'agente o dall'utente.
    description TEXT,

    -- SQL della migration (forward) e rollback opzionale. Utili per audit.
    sql_diff TEXT,
    rollback_sql TEXT,

    -- Audit trail: chi ha creato e chi ha applicato la migration.
    created_by_agent TEXT,                    -- nome agente Nexus che l'ha generata
    created_by_user UUID REFERENCES users(id) ON DELETE SET NULL,
    applied_by_user UUID REFERENCES users(id) ON DELETE SET NULL,
    applied_by_agent TEXT,

    -- Motivo dell'override (se status = 'overridden' o 'pending_override').
    override_reason TEXT,

    -- Messaggio errore se status = 'failed'.
    error_message TEXT,

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    applied_at TIMESTAMPTZ,
    rolled_back_at TIMESTAMPTZ,

    UNIQUE (project_id, filename)
);

CREATE INDEX IF NOT EXISTS idx_project_migration_history_project
    ON project_migration_history(project_id);

CREATE INDEX IF NOT EXISTS idx_project_migration_history_status
    ON project_migration_history(project_id, status);

CREATE INDEX IF NOT EXISTS idx_project_migration_history_created_at
    ON project_migration_history(created_at DESC);

COMMENT ON TABLE project_migration_history IS
    'Storico migrazioni DB per progetto utente. Traccia checksum, stato e audit trail (chi/quando).';
COMMENT ON COLUMN project_migration_history.checksum IS
    'SHA-256 del contenuto del file migration. Rileva modifiche post-apply.';
COMMENT ON COLUMN project_migration_history.sql_diff IS
    'SQL forward della migration per audit. Non contiene dati sensibili.';
