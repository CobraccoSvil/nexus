-- Migrazione 0128: catena di escalation intra-provider
-- Risolve il gap G1: quando si rileva un loop su (provider, model_A),
-- il brain Python scala a model_B dello stesso provider (posizione 1, 2, ...)
-- prima di cambiare provider. Sostituisce il fallback hardcoded claude-sonnet-4-6
-- in brain/agents/nodes.py:916.
--
-- Schema scelto deliberatamente senza FK verso ai_price_catalog:
-- la catena deve sopravvivere a rotazioni del catalogo.

CREATE TABLE IF NOT EXISTS nexus_model_escalation_chain (
    provider              TEXT    NOT NULL,
    base_model            TEXT    NOT NULL,
    escalation_position   INT     NOT NULL CHECK (escalation_position >= 1),
    escalation_model      TEXT    NOT NULL,
    capability_tier       TEXT    NOT NULL CHECK (capability_tier IN ('light','medium','heavy')),
    is_active             BOOLEAN NOT NULL DEFAULT TRUE,
    PRIMARY KEY (provider, base_model, escalation_position)
);

COMMENT ON TABLE nexus_model_escalation_chain IS
    'Catena ordinata di modelli fallback intra-provider. '
    'Usata da executor_node in brain quando rileva un loop per scalare '
    'al modello successivo prima di cambiare provider.';

-- =========================================================
-- SEED: catene per ogni provider
-- Criterio: light → light_superiore → medium → heavy
-- =========================================================

-- --- OpenAI ---
INSERT INTO nexus_model_escalation_chain (provider, base_model, escalation_position, escalation_model, capability_tier) VALUES
-- gpt-4o-mini: scala a gpt-4.1-mini → gpt-4.1 → o4-mini → o3
('openai', 'gpt-4o-mini',   1, 'gpt-4.1-mini', 'light'),
('openai', 'gpt-4o-mini',   2, 'gpt-4.1',      'medium'),
('openai', 'gpt-4o-mini',   3, 'o4-mini',       'medium'),
('openai', 'gpt-4o-mini',   4, 'o3',            'heavy'),
-- gpt-4.1-nano: scala a gpt-4.1-mini → gpt-4.1 → o4-mini
('openai', 'gpt-4.1-nano',  1, 'gpt-4.1-mini', 'light'),
('openai', 'gpt-4.1-nano',  2, 'gpt-4.1',      'medium'),
('openai', 'gpt-4.1-nano',  3, 'o4-mini',       'medium'),
('openai', 'gpt-4.1-nano',  4, 'o3',            'heavy'),
-- gpt-4.1-mini: scala a gpt-4.1 → o4-mini → o3
('openai', 'gpt-4.1-mini',  1, 'gpt-4.1',      'medium'),
('openai', 'gpt-4.1-mini',  2, 'o4-mini',       'medium'),
('openai', 'gpt-4.1-mini',  3, 'o3',            'heavy'),
-- gpt-4.1: scala a o4-mini → o3
('openai', 'gpt-4.1',       1, 'o4-mini',       'medium'),
('openai', 'gpt-4.1',       2, 'o3',            'heavy'),
-- o4-mini: scala a o3
('openai', 'o4-mini',        1, 'o3',            'heavy')
ON CONFLICT DO NOTHING;

-- --- Anthropic ---
INSERT INTO nexus_model_escalation_chain (provider, base_model, escalation_position, escalation_model, capability_tier) VALUES
-- claude-3-haiku (legacy): → claude-haiku-4-5 → claude-sonnet-4-6 → claude-opus-4-6
('anthropic', 'claude-3-haiku-20240307',   1, 'claude-haiku-4-5-20251001', 'light'),
('anthropic', 'claude-3-haiku-20240307',   2, 'claude-sonnet-4-6',         'medium'),
('anthropic', 'claude-3-haiku-20240307',   3, 'claude-opus-4-6',           'heavy'),
-- claude-haiku-4-5: → claude-sonnet-4-6 → claude-opus-4-6
('anthropic', 'claude-haiku-4-5-20251001', 1, 'claude-sonnet-4-6',         'medium'),
('anthropic', 'claude-haiku-4-5-20251001', 2, 'claude-opus-4-6',           'heavy'),
-- claude-sonnet-4-6: → claude-opus-4-6
('anthropic', 'claude-sonnet-4-6',         1, 'claude-opus-4-6',           'heavy')
ON CONFLICT DO NOTHING;

-- --- Mistral ---
INSERT INTO nexus_model_escalation_chain (provider, base_model, escalation_position, escalation_model, capability_tier) VALUES
-- mistral-small-latest: → open-mistral-nemo → codestral-latest → mistral-large-2411
('mistral', 'mistral-small-latest', 1, 'open-mistral-nemo',  'light'),
('mistral', 'mistral-small-latest', 2, 'codestral-latest',   'medium'),
('mistral', 'mistral-small-latest', 3, 'mistral-large-2411', 'medium'),
-- open-mistral-nemo: → codestral-latest → mistral-large-2411
('mistral', 'open-mistral-nemo',    1, 'codestral-latest',   'medium'),
('mistral', 'open-mistral-nemo',    2, 'mistral-large-2411', 'medium'),
-- codestral-latest: → mistral-large-2411
('mistral', 'codestral-latest',     1, 'mistral-large-2411', 'medium')
ON CONFLICT DO NOTHING;

-- --- Google ---
INSERT INTO nexus_model_escalation_chain (provider, base_model, escalation_position, escalation_model, capability_tier) VALUES
-- gemini-2.5-flash-lite: → gemini-2.5-flash → gemini-2.5-pro
('google', 'gemini-2.5-flash-lite', 1, 'gemini-2.5-flash', 'light'),
('google', 'gemini-2.5-flash-lite', 2, 'gemini-2.5-pro',   'medium'),
-- gemini-2.5-flash: → gemini-2.5-pro
('google', 'gemini-2.5-flash',      1, 'gemini-2.5-pro',   'medium'),
-- gemini-2.0-flash: → gemini-2.5-flash → gemini-2.5-pro
('google', 'gemini-2.0-flash',      1, 'gemini-2.5-flash', 'light'),
('google', 'gemini-2.0-flash',      2, 'gemini-2.5-pro',   'medium'),
-- gemini-1.5-flash: → gemini-2.0-flash → gemini-2.5-flash → gemini-2.5-pro
('google', 'gemini-1.5-flash',      1, 'gemini-2.0-flash', 'light'),
('google', 'gemini-1.5-flash',      2, 'gemini-2.5-flash', 'light'),
('google', 'gemini-1.5-flash',      3, 'gemini-2.5-pro',   'medium')
ON CONFLICT DO NOTHING;

-- --- DeepSeek ---
INSERT INTO nexus_model_escalation_chain (provider, base_model, escalation_position, escalation_model, capability_tier) VALUES
-- deepseek-chat: → deepseek-reasoner
('deepseek', 'deepseek-chat',   1, 'deepseek-reasoner', 'heavy'),
-- deepseek-coder: → deepseek-reasoner
('deepseek', 'deepseek-coder',  1, 'deepseek-reasoner', 'heavy')
ON CONFLICT DO NOTHING;
