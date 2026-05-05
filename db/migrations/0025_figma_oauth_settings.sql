-- Figma OAuth + fallback settings for Plugin Manager.

INSERT INTO settings (key, value, category, description, is_secret, updated_at)
VALUES
    ('figma_client_id', '', 'connectors', 'OAuth Client ID Figma per MCP remote', FALSE, NOW()),
    ('figma_client_secret', '', 'connectors', 'OAuth Client Secret Figma per MCP remote', TRUE, NOW()),
    ('figma_oauth_redirect_uri', '', 'connectors', 'Override callback OAuth Figma (default: backend/auth/figma/mcp/callback)', FALSE, NOW()),
    ('figma_oauth_token', '', 'connectors', 'Token Figma (PAT figd_... o OAuth access token) usato dal plugin Figma MCP', TRUE, NOW()),
    ('figma_refresh_token', '', 'connectors', 'Refresh token OAuth Figma', TRUE, NOW()),
    ('figma_token_scope', '', 'connectors', 'Scope token OAuth Figma', FALSE, NOW()),
    ('figma_token_expires_at', '', 'connectors', 'Scadenza token OAuth Figma (ISO8601)', FALSE, NOW()),
    ('figma_last_oauth_error', '', 'connectors', 'Ultimo errore OAuth Figma', FALSE, NOW()),
    ('figma_region', 'www.figma.com', 'connectors', 'Figma region header per MCP HTTP', FALSE, NOW()),
    ('figma_mcp_prefer_stdio', 'true', 'connectors', 'Se true usa fallback stdio (figma-developer-mcp) invece di endpoint MCP HTTP', FALSE, NOW())
ON CONFLICT (key) DO NOTHING;

UPDATE settings
SET category = 'connectors',
    description = 'Token Figma (PAT figd_... o OAuth access token) usato dal plugin Figma MCP',
    is_secret = TRUE,
    updated_at = NOW()
WHERE key = 'figma_oauth_token';

