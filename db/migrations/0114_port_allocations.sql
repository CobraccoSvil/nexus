-- 0114_port_allocations.sql
-- Registro centralizzato delle porte TCP allocate ai progetti.
-- Vincolo UNIQUE su `port` impedisce conflitti anche sotto race condition.

CREATE TABLE IF NOT EXISTS nexus_port_allocations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    port INT NOT NULL CHECK (port > 0 AND port <= 65535),
    label TEXT NOT NULL DEFAULT '',
    allocation_mode TEXT NOT NULL DEFAULT 'auto'
        CHECK (allocation_mode IN ('auto', 'manual')),
    run_config_id UUID REFERENCES run_configurations(id) ON DELETE SET NULL,
    service_unit TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT uq_port UNIQUE (port)
);

CREATE INDEX IF NOT EXISTS idx_port_alloc_project ON nexus_port_allocations(project_id);
