-- Migrazione 0101: registry DB-driven dei modelli AI per il routing.
--
-- Risolve il bug strutturale: i nomi modello AI ('mistral-small-latest',
-- 'gemini-2.5-flash', 'claude-haiku-4-5-20251001', 'gpt-4o-mini', 'deepseek-chat',
-- ecc.) erano hardcoded in:
--   - crates/mcp-core/src/orchestrator.rs (matrice 50+ entry)
--   - crates/mcp-core/src/chat_messages.rs (4 punti)
--   - crates/mcp-core/src/models.rs (matrice duplicata)
--   - crates/mcp-core/src/projects/deep_review.rs
--   - brain/grpc_server/main.py (analyzer chain)
--   - brain/grpc_server/neural_service.py (default fallback)
--
-- Quando un modello viene deprecato (es. Mistral ha rinominato 'mistral-small-4'
-- → 'mistral-small-latest' rompendo la prod con 400 invalid_model), oggi serve
-- patch + redeploy. Con questo registry: UPDATE riga DB, refresh dopo 60s.

-- ── Tabella 1: matrice di routing (intent + behavior_mode) ──────────────────
CREATE TABLE IF NOT EXISTS nexus_routing_matrix (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    intent TEXT NOT NULL,
    behavior_mode TEXT NOT NULL,
    provider TEXT NOT NULL,
    model_id TEXT NOT NULL,
    priority INT NOT NULL DEFAULT 100,
    is_active BOOLEAN NOT NULL DEFAULT true,
    notes TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(intent, behavior_mode, provider)
);

CREATE INDEX IF NOT EXISTS idx_routing_matrix_lookup
    ON nexus_routing_matrix(intent, behavior_mode)
    WHERE is_active = true;

COMMENT ON TABLE nexus_routing_matrix IS
'Mappa (intent, behavior_mode) -> (provider, model). Letta dal Rust orchestrator con cache 60s.';

-- ── Tabella 2: modello di default per provider (fallback) ───────────────────
CREATE TABLE IF NOT EXISTS nexus_provider_default_model (
    provider TEXT PRIMARY KEY,
    model_id TEXT NOT NULL,
    notes TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE nexus_provider_default_model IS
'Modello di default per ogni provider. Usato quando il routing matrix non ha entry per (intent, mode).';

-- ── Seed iniziale: replica della matrice attualmente hardcoded ──────────────
-- Fonte: crates/mcp-core/src/orchestrator.rs::route_model_with_mode (righe ~317-389)
INSERT INTO nexus_routing_matrix (intent, behavior_mode, provider, model_id, notes) VALUES
    -- chat breve
    ('chat_breve',    'veloce',       'google',    'gemini-2.5-flash-lite', 'seed da orchestrator.rs'),
    ('chat_breve',    'economica',    'openai',    'gpt-4.1-nano',          'seed da orchestrator.rs'),
    ('chat_breve',    'bilanciata',   'google',    'gemini-2.5-flash',      'seed da orchestrator.rs'),
    ('chat_breve',    'approfondita', 'mistral',   'mistral-small-latest',  'seed da orchestrator.rs'),
    -- chat media
    ('chat_media',    'veloce',       'openai',    'gpt-4.1-mini',          'seed da orchestrator.rs'),
    ('chat_media',    'economica',    'mistral',   'open-mistral-nemo',     'seed da orchestrator.rs'),
    ('chat_media',    'bilanciata',   'openai',    'gpt-4.1-mini',          'seed da orchestrator.rs'),
    ('chat_media',    'approfondita', 'deepseek',  'deepseek-chat',         'seed da orchestrator.rs'),
    -- chat lunga
    ('chat_lunga',    'veloce',       'mistral',   'mistral-small-latest',  'seed da orchestrator.rs'),
    ('chat_lunga',    'economica',    'deepseek',  'deepseek-chat',         'seed da orchestrator.rs'),
    ('chat_lunga',    'bilanciata',   'anthropic', 'claude-haiku-4-5-20251001', 'seed da orchestrator.rs'),
    ('chat_lunga',    'approfondita', 'anthropic', 'claude-sonnet-4-6',     'seed da orchestrator.rs'),
    -- fix semplice
    ('fix_semplice',  'veloce',       'openai',    'gpt-4.1-mini',          'seed da orchestrator.rs'),
    ('fix_semplice',  'economica',    'openai',    'gpt-4.1-nano',          'seed da orchestrator.rs'),
    ('fix_semplice',  'bilanciata',   'openai',    'gpt-4.1-mini',          'seed da orchestrator.rs'),
    ('fix_semplice',  'approfondita', 'anthropic', 'claude-haiku-4-5-20251001', 'seed da orchestrator.rs'),
    -- fix complesso
    ('fix_complesso', 'veloce',       'deepseek',  'deepseek-chat',         'seed da orchestrator.rs'),
    ('fix_complesso', 'economica',    'deepseek',  'deepseek-chat',         'seed da orchestrator.rs'),
    ('fix_complesso', 'bilanciata',   'anthropic', 'claude-haiku-4-5-20251001', 'seed da orchestrator.rs'),
    ('fix_complesso', 'approfondita', 'anthropic', 'claude-sonnet-4-6',     'seed da orchestrator.rs'),
    -- refactor
    ('refactor',      'veloce',       'deepseek',  'deepseek-chat',         'seed da orchestrator.rs'),
    ('refactor',      'economica',    'deepseek',  'deepseek-chat',         'seed da orchestrator.rs'),
    ('refactor',      'bilanciata',   'anthropic', 'claude-haiku-4-5-20251001', 'seed da orchestrator.rs'),
    ('refactor',      'approfondita', 'anthropic', 'claude-sonnet-4-6',     'seed da orchestrator.rs'),
    -- test
    ('test',          'veloce',       'openai',    'gpt-4.1-mini',          'seed da orchestrator.rs'),
    ('test',          'economica',    'mistral',   'open-mistral-nemo',     'seed da orchestrator.rs'),
    ('test',          'bilanciata',   'openai',    'gpt-4.1-mini',          'seed da orchestrator.rs'),
    ('test',          'approfondita', 'mistral',   'codestral-latest',      'seed da orchestrator.rs'),
    -- docs
    ('docs',          'veloce',       'mistral',   'mistral-small-latest',  'seed da orchestrator.rs'),
    ('docs',          'economica',    'mistral',   'open-mistral-nemo',     'seed da orchestrator.rs'),
    ('docs',          'bilanciata',   'openai',    'gpt-4.1',               'seed da orchestrator.rs'),
    ('docs',          'approfondita', 'anthropic', 'claude-sonnet-4-6',     'seed da orchestrator.rs'),
    -- architecture
    ('architecture',  'veloce',       'deepseek',  'deepseek-reasoner',     'seed da orchestrator.rs'),
    ('architecture',  'economica',    'deepseek',  'deepseek-chat',         'seed da orchestrator.rs'),
    ('architecture',  'bilanciata',   'anthropic', 'claude-sonnet-4-6',     'seed da orchestrator.rs'),
    ('architecture',  'approfondita', 'anthropic', 'claude-opus-4-6',       'seed da orchestrator.rs'),
    -- debug
    ('debug',         'veloce',       'anthropic', 'claude-haiku-4-5-20251001', 'seed da orchestrator.rs'),
    ('debug',         'economica',    'anthropic', 'claude-haiku-4-5-20251001', 'seed da orchestrator.rs'),
    ('debug',         'bilanciata',   'anthropic', 'claude-sonnet-4-6',     'seed da orchestrator.rs'),
    ('debug',         'approfondita', 'anthropic', 'claude-opus-4-6',       'seed da orchestrator.rs'),
    -- file_ops (richiede tool use solido, no modelli "lite")
    ('file_ops',      'veloce',       'openai',    'gpt-4.1-mini',          'seed da orchestrator.rs'),
    ('file_ops',      'economica',    'anthropic', 'claude-haiku-4-5-20251001', 'seed da orchestrator.rs'),
    ('file_ops',      'bilanciata',   'anthropic', 'claude-haiku-4-5-20251001', 'seed da orchestrator.rs'),
    ('file_ops',      'approfondita', 'anthropic', 'claude-sonnet-4-6',     'seed da orchestrator.rs'),
    -- system_admin (side-effect importanti, solo modelli capable)
    ('system_admin',  'veloce',       'anthropic', 'claude-haiku-4-5-20251001', 'seed da orchestrator.rs'),
    ('system_admin',  'economica',    'anthropic', 'claude-haiku-4-5-20251001', 'seed da orchestrator.rs'),
    ('system_admin',  'bilanciata',   'anthropic', 'claude-sonnet-4-6',     'seed da orchestrator.rs'),
    ('system_admin',  'approfondita', 'anthropic', 'claude-sonnet-4-6',     'seed da orchestrator.rs')
ON CONFLICT (intent, behavior_mode, provider) DO NOTHING;

-- Default per-provider (usato da default_model_for_provider)
INSERT INTO nexus_provider_default_model (provider, model_id, notes) VALUES
    ('openai',    'gpt-4o-mini',                   'seed da orchestrator.rs::default_model_for_provider'),
    ('anthropic', 'claude-sonnet-4-6',             'seed da orchestrator.rs::default_model_for_provider'),
    ('google',    'gemini-2.5-flash',              'seed da orchestrator.rs::default_model_for_provider'),
    ('mistral',   'mistral-small-latest',          'seed da orchestrator.rs::default_model_for_provider'),
    ('deepseek',  'deepseek-chat',                 'seed da orchestrator.rs::default_model_for_provider')
ON CONFLICT (provider) DO NOTHING;
