CREATE TABLE IF NOT EXISTS agent_processes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    session_id UUID REFERENCES chat_sessions(id) ON DELETE SET NULL,
    label TEXT NOT NULL DEFAULT '',
    command TEXT NOT NULL,
    working_dir TEXT,
    pid INTEGER,
    status TEXT NOT NULL DEFAULT 'starting',  -- starting | running | stopped | failed
    exit_code INTEGER,
    output TEXT NOT NULL DEFAULT '',
    error_output TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    stopped_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_agent_processes_project ON agent_processes(project_id, status);
CREATE INDEX IF NOT EXISTS idx_agent_processes_created ON agent_processes(project_id, created_at DESC);
