-- GitHub Source Control+: per-user GitHub connection for IDE Git operations

CREATE TABLE IF NOT EXISTS github_connections (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    github_user_id BIGINT,
    github_username TEXT,
    connection_status TEXT NOT NULL DEFAULT 'connected'
        CHECK (connection_status IN ('connected', 'disconnected')),
    access_token_encrypted BYTEA,
    refresh_token_encrypted BYTEA,
    token_scope TEXT NOT NULL DEFAULT '',
    access_token_expires_at TIMESTAMPTZ,
    refresh_token_expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_github_connections_status
    ON github_connections(connection_status, updated_at DESC);

INSERT INTO settings (key, value, category, description, is_secret)
VALUES
    (
        'oauth_data_encryption_key',
        '',
        'auth',
        'Secret per cifrare i token OAuth salvati a riposo',
        TRUE
    )
ON CONFLICT (key) DO NOTHING;
