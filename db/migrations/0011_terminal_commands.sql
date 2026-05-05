-- Comandi da iniettare nei terminali IDE via agente
CREATE TABLE IF NOT EXISTS terminal_commands (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    session_id UUID REFERENCES chat_sessions(id) ON DELETE SET NULL,
    command TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending', -- pending | delivered | expired
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    delivered_at TIMESTAMPTZ
);
CREATE INDEX idx_terminal_commands_project ON terminal_commands(project_id, status, created_at);
