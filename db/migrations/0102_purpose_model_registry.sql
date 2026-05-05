-- Migrazione 0102: registry DB-driven per "purpose-specific" models.
--
-- Estende il pattern del registry 0101 ai modelli usati per task interni
-- (non routing utente) attualmente hardcoded:
--   - prompt_templates.rs:462           -> default per generate_with_admin_fallback
--   - orchestrator.rs:1327              -> fallback model in resolve_agent_provider
--   - chat_messages.rs:2608, 2707       -> chat-title/feedback generator (gpt-4.1-nano)
--   - nexus_builtin/docs.rs:134         -> docs generator (gpt-4.1-nano)
--   - projects/custom_instructions.rs   -> custom instructions generator (claude-haiku)
--   - semantic_compact.rs:15            -> COMPACTOR_MODEL constante (claude-haiku)
--   - projects/deep_review.rs           -> google batch fallback (gemini-2.5-flash)
--
-- Schema: stessa struttura di nexus_provider_default_model ma con chiave
-- "purpose" invece di provider. Cosi' un UPDATE in DB rimpiazza un modello
-- senza patch + redeploy.

CREATE TABLE IF NOT EXISTS nexus_purpose_model (
    purpose TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    model_id TEXT NOT NULL,
    notes TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE nexus_purpose_model IS
'Modello per task interni (non routing utente): chat title, doc generator, compactor, custom instructions, ecc. Letto dal Rust con cache 60s (vedi routing_matrix.rs).';

-- Seed con i valori attualmente hardcoded.
-- Nota: economici/veloci per task ripetitivi (title, feedback), capable per task con tool use.
INSERT INTO nexus_purpose_model (purpose, provider, model_id, notes) VALUES
    ('chat_title_generator',     'openai',    'gpt-4.1-nano',              'seed: chat_messages.rs:2608'),
    ('chat_feedback_generator',  'openai',    'gpt-4.1-nano',              'seed: chat_messages.rs:2707'),
    ('docs_generator',           'openai',    'gpt-4.1-nano',              'seed: nexus_builtin/docs.rs:134'),
    ('custom_instructions',      'anthropic', 'claude-haiku-4-5-20251001', 'seed: projects/custom_instructions.rs:284'),
    ('admin_fallback_default',   'anthropic', 'claude-haiku-4-5-20251001', 'seed: prompt_templates.rs:462'),
    ('google_batch',             'google',    'gemini-2.5-flash',          'seed: projects/deep_review.rs')
ON CONFLICT (purpose) DO NOTHING;
