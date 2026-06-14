-- 0418_gateway_rust_settings.sql
-- Fase 6 della migrazione del gateway LLM da Node a Rust (crate nexus-gateway).
-- Seed dei default delle settings lette dal gateway Rust:
--   - cooldown billing con re-probe reattivo (il fix "OpenAI non torna dopo la
--     ricarica": il provider rientra entro reprobe_interval, non dopo ore);
--   - client Presidio per la pipeline DLP (vuoto = non configurato, fallback
--     graceful: il secret scanner copre comunque i segreti strutturati).
-- Il gateway li ricarica a caldo (TtlCache 60s), quindi l'admin puo'
-- sovrascriverli senza restart. Idempotente: i valori restano se gia' presenti.

INSERT INTO settings (key, value) VALUES
  ('gateway.cooldown.billing_seconds', '3600'),
  ('gateway.cooldown.transient_seconds', '30'),
  ('gateway.cooldown.reprobe_interval_seconds', '600'),
  ('dlp_presidio_base_url', ''),
  ('dlp_presidio_language', 'it'),
  ('dlp_presidio_timeout_ms', '5000')
ON CONFLICT (key) DO NOTHING;
