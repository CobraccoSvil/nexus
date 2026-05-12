-- 0136: Aggiunge purpose_model per prompt_optimizer e anthropic_batch.
--
-- Questi due purpose erano usati con modelli hardcoded nel codice.
-- Ora il codice li risolve da nexus_purpose_model (niente fallback hardcoded).
-- Vedi: crates/nexus-orchestrator/src/workers/prompt_optimizer.rs
--       brain/providers/anthropic_batch.py

INSERT INTO nexus_purpose_model (purpose, provider, model_id) VALUES
  ('prompt_optimizer', 'anthropic', 'claude-haiku-4-5-20251001'),
  ('anthropic_batch',  'anthropic', 'claude-haiku-4-5-20251001')
ON CONFLICT (purpose) DO NOTHING;
