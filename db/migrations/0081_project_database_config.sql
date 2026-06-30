-- Configurazione database per-progetto utente.
-- Ogni progetto importato in Nexus puo' dichiarare il proprio motore DB,
-- la modalita' di hosting (interno gestito da Nexus vs server esterno),
-- e il migration tool rilevato dal detector (alembic, prisma, sqlx, ...).
--
-- Nota di sicurezza: connection_secret e' cifrato con la stessa chiave
-- usata per provider_configs (vedi crates/mcp-core/src/secrets.rs).
-- Non inserire mai credenziali in chiaro in questa tabella.

CREATE TABLE IF NOT EXISTS project_database_config (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,

    -- Motore DB: 'postgres' (default V1), 'mysql', 'sqlite', 'mongodb', ...
    engine TEXT NOT NULL,

    -- Hosting: 'internal' = container Docker gestito da Nexus;
    --          'external' = server fornito dall'utente.
    hosting_mode TEXT NOT NULL CHECK (hosting_mode IN ('internal', 'external')),

    -- Connessione cifrata (host, port, database, user, password).
    -- Formato interno: JSONB cifrato -> BYTEA.
    connection_secret BYTEA,

    -- Tool di migrazione rilevato o scelto dall'utente.
    -- Valori: alembic | prisma | sqlx | flyway | django | rails | knex | liquibase | generic-sql
    migration_tool TEXT,

    -- Path relativo alla directory migrations nel progetto (es. "migrations/", "prisma/migrations/").
    migration_path TEXT,

    -- Se true, l'utente ha concesso l'override per eseguire DDL diretto (hard-block disabilitato).
    allow_ddl_override BOOLEAN NOT NULL DEFAULT false,

    -- Metadati rilevamento: confidence score, marker file trovati, etc.
    detection_metadata JSONB NOT NULL DEFAULT '{}'::jsonb,

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    UNIQUE (project_id)
);

CREATE INDEX IF NOT EXISTS idx_project_database_config_project
    ON project_database_config(project_id);

CREATE INDEX IF NOT EXISTS idx_project_database_config_engine
    ON project_database_config(engine);

COMMENT ON TABLE project_database_config IS
    'Configurazione DB per-progetto utente: motore, hosting, migration tool. Non si applica al DB interno di Nexus.';
COMMENT ON COLUMN project_database_config.connection_secret IS
    'Credenziali cifrate (stessa chiave di provider_configs). NULL per hosting interno fino al primo avvio container.';
COMMENT ON COLUMN project_database_config.allow_ddl_override IS
    'Se true il guardrail DDL e'' disabilitato per questo progetto (richiede conferma UI).';
