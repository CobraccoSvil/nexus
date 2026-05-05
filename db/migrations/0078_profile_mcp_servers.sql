-- Associazione many-to-many tra profili di sistema e server MCP globali.
-- Un profilo può abilitare specifici server MCP che vengono caricati
-- automaticamente quando il profilo è attivo in una sessione chat.

CREATE TABLE IF NOT EXISTS profile_mcp_servers (
    profile_id    UUID NOT NULL REFERENCES user_profiles(id) ON DELETE CASCADE,
    mcp_server_id UUID NOT NULL REFERENCES mcp_servers(id)   ON DELETE CASCADE,
    PRIMARY KEY (profile_id, mcp_server_id)
);

CREATE INDEX IF NOT EXISTS idx_profile_mcp_servers_profile
    ON profile_mcp_servers(profile_id);

CREATE INDEX IF NOT EXISTS idx_profile_mcp_servers_server
    ON profile_mcp_servers(mcp_server_id);
