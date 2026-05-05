-- GitHub OAuth: add GitHub fields to users, create sessions table, seed auth settings

ALTER TABLE users ADD COLUMN IF NOT EXISTS github_id BIGINT UNIQUE;
ALTER TABLE users ADD COLUMN IF NOT EXISTS github_username TEXT;
ALTER TABLE users ADD COLUMN IF NOT EXISTS avatar_url TEXT;

CREATE TABLE IF NOT EXISTS sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_sessions_token_hash ON sessions(token_hash);
CREATE INDEX IF NOT EXISTS idx_sessions_expires_at ON sessions(expires_at);

INSERT INTO settings (key, value, category, description, is_secret)
VALUES
    ('github_client_id', '', 'auth', 'GitHub OAuth App Client ID', FALSE),
    ('github_client_secret', '', 'auth', 'GitHub OAuth App Client Secret', TRUE),
    ('jwt_secret', '', 'auth', 'JWT signing secret (auto-generated on first login)', TRUE)
ON CONFLICT (key) DO NOTHING;
