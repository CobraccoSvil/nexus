-- Agent runs: traccia ogni esecuzione del loop agente
CREATE TABLE IF NOT EXISTS agent_runs (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id           UUID NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    project_id           UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    user_id              UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    run_message_id       UUID REFERENCES chat_messages(id) ON DELETE SET NULL,
    status               TEXT NOT NULL DEFAULT 'running',
    automation_mode      TEXT NOT NULL DEFAULT 'confirm',
    provider             TEXT,
    model                TEXT,
    iteration_count      INT  NOT NULL DEFAULT 0,
    final_answer         TEXT,
    pending_actions_json JSONB,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at         TIMESTAMPTZ
);

-- Agent steps: singoli passi tool all'interno di un run
CREATE TABLE IF NOT EXISTS agent_steps (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    run_id      UUID NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
    step_index  INT  NOT NULL,
    tool_name   TEXT NOT NULL,
    tool_input  JSONB NOT NULL,
    tool_result TEXT,
    status      TEXT NOT NULL DEFAULT 'running',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_agent_steps_run_id ON agent_steps(run_id, step_index);
CREATE INDEX IF NOT EXISTS idx_agent_runs_session  ON agent_runs(session_id);
CREATE INDEX IF NOT EXISTS idx_agent_runs_user     ON agent_runs(user_id);
