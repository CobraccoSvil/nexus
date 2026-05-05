-- Migration 0044: Nexus Builtin MCP Server
-- Aggiunge 'builtin' come tipo di transport per il server MCP interno di Nexus

ALTER TABLE mcp_servers DROP CONSTRAINT IF EXISTS mcp_servers_transport_check;
ALTER TABLE mcp_servers ADD CONSTRAINT mcp_servers_transport_check
    CHECK (transport IN ('http', 'stdio', 'builtin'));

-- Pre-inserisce il server Nexus Builtin con UUID fisso e deterministico
-- I tool vengono upsertati in Rust all'avvio tramite seed_tools_and_server()
INSERT INTO mcp_servers (id, name, description, transport, enabled, scope)
VALUES (
    '00000000-0000-0000-0000-000000000001',
    'Nexus Builtin',
    'Tool integrati della piattaforma Nexus: run config, profili, git avanzato, qualità progetto, admin settings',
    'builtin',
    true,
    'global'
)
ON CONFLICT (id) DO NOTHING;
