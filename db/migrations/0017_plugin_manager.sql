-- Plugin Manager v1 (Plugin = MCP)

CREATE TABLE IF NOT EXISTS plugin_catalog_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    slug TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    plugin_type TEXT NOT NULL DEFAULT 'mcp' CHECK (plugin_type IN ('mcp')),
    transport TEXT NOT NULL CHECK (transport IN ('http', 'stdio')),
    http_url TEXT,
    stdio_command TEXT,
    stdio_args JSONB NOT NULL DEFAULT '[]',
    required_secret_refs JSONB NOT NULL DEFAULT '[]',
    optional_secret_refs JSONB NOT NULL DEFAULT '[]',
    default_scope TEXT NOT NULL DEFAULT 'global' CHECK (default_scope IN ('user', 'project', 'global')),
    allowed_commands JSONB NOT NULL DEFAULT '[]',
    default_tool_policy JSONB NOT NULL DEFAULT '{"mode":"allowlist","tools":[],"blockedTools":[]}',
    metadata JSONB NOT NULL DEFAULT '{}',
    is_allowlisted BOOLEAN NOT NULL DEFAULT TRUE,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS plugin_releases (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    catalog_item_id UUID NOT NULL REFERENCES plugin_catalog_items(id) ON DELETE CASCADE,
    version TEXT NOT NULL,
    changelog TEXT NOT NULL DEFAULT '',
    config_patch JSONB NOT NULL DEFAULT '{}',
    is_stable BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (catalog_item_id, version)
);

CREATE TABLE IF NOT EXISTS plugin_instances (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    catalog_item_id UUID NOT NULL REFERENCES plugin_catalog_items(id) ON DELETE RESTRICT,
    release_id UUID REFERENCES plugin_releases(id) ON DELETE SET NULL,
    installed_by_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    project_id UUID REFERENCES projects(id) ON DELETE CASCADE,
    scope TEXT NOT NULL DEFAULT 'global' CHECK (scope IN ('user', 'project', 'global')),
    name TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    config JSONB NOT NULL DEFAULT '{}',
    secret_bindings JSONB NOT NULL DEFAULT '{}',
    health_status TEXT NOT NULL DEFAULT 'unknown' CHECK (health_status IN ('unknown', 'ok', 'error')),
    last_health_message TEXT,
    last_tested_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS plugin_instance_tool_policies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    plugin_instance_id UUID NOT NULL UNIQUE REFERENCES plugin_instances(id) ON DELETE CASCADE,
    mode TEXT NOT NULL DEFAULT 'allowlist' CHECK (mode IN ('allowlist', 'denylist', 'all')),
    tools JSONB NOT NULL DEFAULT '[]',
    blocked_tools JSONB NOT NULL DEFAULT '[]',
    updated_by_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS plugin_instance_health_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    plugin_instance_id UUID NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
    tested_by_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    success BOOLEAN NOT NULL,
    tool_count INTEGER NOT NULL DEFAULT 0,
    error_message TEXT,
    details JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS plugin_audit_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    plugin_instance_id UUID REFERENCES plugin_instances(id) ON DELETE CASCADE,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    project_id UUID REFERENCES projects(id) ON DELETE SET NULL,
    action TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'ok',
    message TEXT,
    payload JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

ALTER TABLE mcp_servers
    ADD COLUMN IF NOT EXISTS plugin_instance_id UUID REFERENCES plugin_instances(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_plugin_catalog_slug ON plugin_catalog_items(slug);
CREATE INDEX IF NOT EXISTS idx_plugin_releases_catalog ON plugin_releases(catalog_item_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_plugin_instances_scope ON plugin_instances(scope, enabled);
CREATE INDEX IF NOT EXISTS idx_plugin_instances_project ON plugin_instances(project_id);
CREATE INDEX IF NOT EXISTS idx_plugin_instances_catalog ON plugin_instances(catalog_item_id);
CREATE INDEX IF NOT EXISTS idx_plugin_health_runs_instance ON plugin_instance_health_runs(plugin_instance_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_plugin_audit_instance ON plugin_audit_events(plugin_instance_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_plugin_audit_user ON plugin_audit_events(user_id, created_at DESC);

INSERT INTO settings (key, value, category, description, is_secret)
VALUES
    ('figma_oauth_token', '', 'connectors', 'Token OAuth Figma per plugin MCP Figma', TRUE),
    ('figma_region', 'us-east-1', 'connectors', 'Header X-Figma-Region per plugin MCP Figma', FALSE)
ON CONFLICT (key) DO NOTHING;

-- Catalog bootstrap
WITH upsert_catalog AS (
    INSERT INTO plugin_catalog_items (
        slug, name, description, plugin_type, transport, http_url, stdio_command, stdio_args,
        required_secret_refs, optional_secret_refs, default_scope, allowed_commands,
        default_tool_policy, metadata, is_allowlisted, enabled
    )
    VALUES
    (
        'figma-http',
        'Figma (HTTP)',
        'Acquisizione design context e layout da Figma MCP.',
        'mcp',
        'http',
        'https://mcp.figma.com/mcp',
        NULL,
        '[]',
        '["figma_oauth_token"]',
        '["figma_region"]',
        'global',
        '[]',
        '{"mode":"allowlist","tools":["get_design_context","get_metadata","get_screenshot","get_variable_defs","search_design_system","get_context_for_code_connect"],"blockedTools":["use_figma","generate_figma_design"]}',
        '{"docsUrl":"https://www.figma.com","category":"design"}',
        TRUE,
        TRUE
    ),
    (
        'github-http',
        'GitHub (HTTP)',
        'Accesso repository GitHub via MCP HTTP.',
        'mcp',
        'http',
        'https://api.githubcopilot.com/mcp',
        NULL,
        '[]',
        '["github_personal_access_token"]',
        '[]',
        'global',
        '[]',
        '{"mode":"all","tools":[],"blockedTools":[]}',
        '{"docsUrl":"https://github.com/github/github-mcp-server","category":"version-control"}',
        TRUE,
        TRUE
    ),
    (
        'filesystem-local',
        'Filesystem',
        'Accesso filesystem locale server tramite MCP stdio.',
        'mcp',
        'stdio',
        NULL,
        'npx',
        '["-y","@modelcontextprotocol/server-filesystem","/"]',
        '[]',
        '[]',
        'global',
        '["npx"]',
        '{"mode":"all","tools":[],"blockedTools":[]}',
        '{"docsUrl":"https://github.com/modelcontextprotocol/servers/tree/main/src/filesystem","category":"filesystem"}',
        TRUE,
        TRUE
    ),
    (
        'playwright-stdio',
        'Playwright (Browser)',
        'Automazione browser con Playwright MCP.',
        'mcp',
        'stdio',
        NULL,
        'npx',
        '["@playwright/mcp@latest"]',
        '[]',
        '[]',
        'global',
        '["npx"]',
        '{"mode":"all","tools":[],"blockedTools":[]}',
        '{"docsUrl":"https://github.com/microsoft/playwright-mcp","category":"dev-tools"}',
        TRUE,
        TRUE
    )
    ON CONFLICT (slug) DO UPDATE
    SET
        name = EXCLUDED.name,
        description = EXCLUDED.description,
        plugin_type = EXCLUDED.plugin_type,
        transport = EXCLUDED.transport,
        http_url = EXCLUDED.http_url,
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
    RETURNING id, slug
)
INSERT INTO plugin_releases (catalog_item_id, version, changelog, config_patch, is_stable)
SELECT
    uc.id,
    '1.0.0',
    'Bootstrap release',
    '{}',
    TRUE
FROM upsert_catalog uc
ON CONFLICT (catalog_item_id, version) DO NOTHING;
