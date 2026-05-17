-- Dispatcher centrale: flag globali per progetto.
-- Settati dall'agente via tool `dispatcher_set_flag`.
-- Letti dal frontend tramite il bootstrap snapshot REST.

CREATE TABLE IF NOT EXISTS nexus_project_flags (
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    key        TEXT NOT NULL,
    value      JSONB,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (project_id, key)
);

CREATE INDEX IF NOT EXISTS idx_project_flags_project ON nexus_project_flags(project_id);
