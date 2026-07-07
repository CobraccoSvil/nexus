-- Purpose model usato per auto-escalation quando il brain rileva loop tool-use.
-- DB-driven: modificabile da admin UI tramite nexus_purpose_model.
--
-- Default: anthropic / claude-sonnet-4-6 (se disponibile).

INSERT INTO nexus_purpose_model (purpose, provider, model_id)
VALUES ('loop_fallback_default', 'anthropic', 'claude-sonnet-4-6')
ON CONFLICT (purpose)
DO UPDATE SET provider = EXCLUDED.provider, model_id = EXCLUDED.model_id, updated_at = NOW();

