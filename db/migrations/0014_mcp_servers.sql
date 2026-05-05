-- MCP external server connectors
CREATE TABLE IF NOT EXISTS mcp_servers (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     UUID REFERENCES users(id) ON DELETE CASCADE,
    project_id  UUID REFERENCES projects(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    description TEXT,
    icon_url    TEXT,
    transport   TEXT NOT NULL CHECK (transport IN ('http', 'stdio')),
    url         TEXT,                          -- per HTTP
    command     TEXT,                          -- per stdio (es. "npx")
    args        JSONB NOT NULL DEFAULT '[]',   -- per stdio (es. ["-y","airtable-mcp-server"])
    env_vars    JSONB NOT NULL DEFAULT '{}',   -- variabili d'ambiente extra
    headers     JSONB NOT NULL DEFAULT '{}',   -- headers HTTP (es. Authorization)
    enabled     BOOLEAN NOT NULL DEFAULT true,
    scope       TEXT NOT NULL DEFAULT 'user' CHECK (scope IN ('user', 'project', 'global')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Cache delle tool definitions scoperte da ogni server
CREATE TABLE IF NOT EXISTS mcp_server_tools (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    server_id   UUID NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,
    tool_name   TEXT NOT NULL,
    description TEXT,
    input_schema JSONB NOT NULL DEFAULT '{}',
    discovered_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (server_id, tool_name)
);

CREATE INDEX IF NOT EXISTS idx_mcp_servers_user    ON mcp_servers(user_id);
CREATE INDEX IF NOT EXISTS idx_mcp_servers_project ON mcp_servers(project_id);
CREATE INDEX IF NOT EXISTS idx_mcp_server_tools_server ON mcp_server_tools(server_id);
