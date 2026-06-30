-- Settings table: chiave/valore con categorie e cifratura opzionale
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL DEFAULT '',
    category TEXT NOT NULL DEFAULT 'general',
    description TEXT NOT NULL DEFAULT '',
    is_secret BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Seed default settings
INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('openai_api_key', '', 'providers', 'OpenAI API Key', TRUE),
    ('anthropic_api_key', '', 'providers', 'Anthropic API Key', TRUE),
    ('google_api_key', '', 'providers', 'Google AI API Key', TRUE),
    ('default_provider', 'anthropic', 'routing', 'Default LLM provider', FALSE),
    ('default_model', 'claude-sonnet-4-6', 'routing', 'Default model for chat', FALSE),
    ('token_budget', '4096', 'routing', 'Default token budget per request', FALSE),
    ('max_token_budget', '32000', 'routing', 'Maximum token budget allowed', FALSE),
    ('neural_core_url', 'http://localhost:50051', 'infrastructure', 'Neural Core gRPC URL', FALSE),
    ('redis_url', 'redis://localhost:6379', 'infrastructure', 'Redis connection URL', FALSE),
    ('qdrant_url', 'http://localhost:6333', 'infrastructure', 'Qdrant vector DB URL', FALSE),
    ('qdrant_collection', 'code_embeddings', 'infrastructure', 'Qdrant collection name', FALSE),
    ('embedding_model', 'all-MiniLM-L6-v2', 'embeddings', 'Sentence-transformers model', FALSE),
    ('quality_auto_scan', 'true', 'quality', 'Auto-scan files on save', FALSE),
    ('quality_severity_threshold', 'medium', 'quality', 'Minimum severity to report (low/medium/high)', FALSE),
    ('learning_auto_extract', 'true', 'learning', 'Auto-extract patterns from code', FALSE),
    ('learning_min_confidence', '0.6', 'learning', 'Minimum pattern confidence to keep', FALSE)
ON CONFLICT (key) DO NOTHING;
