CREATE TABLE IF NOT EXISTS run_configurations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    label TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'shell',   -- 'shell' | 'npm' | 'cargo' | 'python' | 'node'
    command TEXT NOT NULL,
    args TEXT[] NOT NULL DEFAULT '{}',
    cwd TEXT,
    env JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_run_configurations_project ON run_configurations(project_id);
