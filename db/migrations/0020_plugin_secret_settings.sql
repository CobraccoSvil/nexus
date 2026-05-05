-- Plugin Manager: chiavi segrete base per plugin MCP curati

INSERT INTO settings (key, value, category, description, is_secret, updated_at)
VALUES
    ('github_personal_access_token', '', 'connectors', 'Token GitHub personale per plugin GitHub MCP (Authorization: Bearer ...)', TRUE, NOW()),
    ('github_token', '', 'connectors', 'Alias token GitHub per integrazioni MCP/legacy', TRUE, NOW()),
    ('gitlab_personal_access_token', '', 'connectors', 'Token GitLab personale per plugin GitLab MCP', TRUE, NOW())
ON CONFLICT (key) DO NOTHING;

