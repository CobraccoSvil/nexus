-- Estende il catalogo curato con MCP stdio standard @modelcontextprotocol/server-*.
-- Obiettivo: rendere migrabili automaticamente i legacy MCP "npx @modelcontextprotocol/server-<name>".

WITH upsert_catalog AS (
    INSERT INTO plugin_catalog_items (
        slug, name, description, plugin_type, transport,
        http_url, stdio_command, stdio_args,
        required_secret_refs, optional_secret_refs,
        default_scope, allowed_commands, default_tool_policy,
        metadata, is_allowlisted, enabled
    )
    VALUES
    (
        'redis-stdio',
        'Redis (stdio)',
        'Redis MCP server (stdio) tramite npx @modelcontextprotocol/server-redis.',
        'mcp',
        'stdio',
        NULL,
        'npx',
        '["-y","@modelcontextprotocol/server-redis"]',
        '[]',
        '[]',
        'global',
        '["npx"]',
        '{"mode":"all","tools":[],"blockedTools":[]}',
        '{"docsUrl":"https://github.com/modelcontextprotocol/servers","category":"database"}',
        TRUE,
        TRUE
    ),
    (
        'sqlite-stdio',
        'SQLite (stdio)',
        'SQLite MCP server (stdio) tramite npx @modelcontextprotocol/server-sqlite.',
        'mcp',
        'stdio',
        NULL,
        'npx',
        '["-y","@modelcontextprotocol/server-sqlite"]',
        '[]',
        '[]',
        'global',
        '["npx"]',
        '{"mode":"all","tools":[],"blockedTools":[]}',
        '{"docsUrl":"https://github.com/modelcontextprotocol/servers","category":"database"}',
        TRUE,
        TRUE
    ),
    (
        'postgres-stdio',
        'PostgreSQL (stdio)',
        'Postgres MCP server (stdio) tramite npx @modelcontextprotocol/server-postgres.',
        'mcp',
        'stdio',
        NULL,
        'npx',
        '["-y","@modelcontextprotocol/server-postgres"]',
        '[]',
        '[]',
        'global',
        '["npx"]',
        '{"mode":"all","tools":[],"blockedTools":[]}',
        '{"docsUrl":"https://github.com/modelcontextprotocol/servers","category":"database"}',
        TRUE,
        TRUE
    ),
    (
        'gitlab-stdio',
        'GitLab (stdio)',
        'GitLab MCP server (stdio) tramite npx @modelcontextprotocol/server-gitlab.',
        'mcp',
        'stdio',
        NULL,
        'npx',
        '["-y","@modelcontextprotocol/server-gitlab"]',
        '[]',
        '[]',
        'global',
        '["npx"]',
        '{"mode":"all","tools":[],"blockedTools":[]}',
        '{"docsUrl":"https://github.com/modelcontextprotocol/servers","category":"version-control"}',
        TRUE,
        TRUE
    ),
    (
        'github-stdio',
        'GitHub (stdio)',
        'GitHub MCP server (stdio) tramite npx @modelcontextprotocol/server-github.',
        'mcp',
        'stdio',
        NULL,
        'npx',
        '["-y","@modelcontextprotocol/server-github"]',
        '[]',
        '[]',
        'global',
        '["npx"]',
        '{"mode":"all","tools":[],"blockedTools":[]}',
        '{"docsUrl":"https://github.com/modelcontextprotocol/servers","category":"version-control"}',
        TRUE,
        TRUE
    ),
    (
        'memory-stdio',
        'Memory (Knowledge Graph) (stdio)',
        'Memory/Knowledge Graph MCP server (stdio) tramite npx @modelcontextprotocol/server-memory.',
        'mcp',
        'stdio',
        NULL,
        'npx',
        '["-y","@modelcontextprotocol/server-memory"]',
        '[]',
        '[]',
        'global',
        '["npx"]',
        '{"mode":"all","tools":[],"blockedTools":[]}',
        '{"docsUrl":"https://github.com/modelcontextprotocol/servers","category":"memory"}',
        TRUE,
        TRUE
    )
    ON CONFLICT (slug) DO UPDATE
    SET
        name = EXCLUDED.name,
        description = EXCLUDED.description,
        transport = EXCLUDED.transport,
        stdio_command = EXCLUDED.stdio_command,
        stdio_args = EXCLUDED.stdio_args,
        required_secret_refs = EXCLUDED.required_secret_refs,
        optional_secret_refs = EXCLUDED.optional_secret_refs,
        default_scope = EXCLUDED.default_scope,
        allowed_commands = EXCLUDED.allowed_commands,
        default_tool_policy = EXCLUDED.default_tool_policy,
        metadata = EXCLUDED.metadata,
        is_allowlisted = EXCLUDED.is_allowlisted,
        enabled = EXCLUDED.enabled,
        updated_at = NOW()
    RETURNING id
)
INSERT INTO plugin_releases (catalog_item_id, version, changelog, config_patch, is_stable)
SELECT
    uc.id,
    '1.0.0',
    'Bootstrap curated MCP stdio server',
    '{}'::jsonb,
    TRUE
FROM upsert_catalog uc
ON CONFLICT (catalog_item_id, version) DO NOTHING;

