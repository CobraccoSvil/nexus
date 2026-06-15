-- 0421_seed_gateway_phantom_settings.sql
-- Seed delle settings gateway lette dal codice ma MAI inserite in DB dalla
-- migrazione 0418 (omissione). L'audit settings (scripts/audit_settings.py) le
-- classifica come "fantasma" (lette in code, assenti dal DB) -> il gate ratchet
-- di pnpm verify falliva ({'fantasma': (5, 0)}).
--
-- Regola G: la configurazione vive nel DB. I valori seedati COINCIDONO con i
-- default gia' usati come fallback dal codice, quindi ZERO cambio di comportamento
-- (la lettura ora trova il valore in DB invece del fallback identico):
--   - nexus_gateway_url            -> _DEV_GATEWAY_URL (gateway_provider.py:60)
--   - gateway.complete_timeout_seconds -> _DEFAULT_COMPLETE_TIMEOUT_S = 120 (:64)
--   - gateway.stream_timeout_seconds   -> _DEFAULT_STREAM_TIMEOUT_S   = 300 (:65)
--   - anthropic_base_url           -> ANTHROPIC_DEFAULT_BASE_URL (routes.rs:614)
--   - vllm_base_url                -> '' (onprem, non configurato nel profilo cloud):
--     nexus_auth::get_setting filtra i valori vuoti e ritorna None, quindi il
--     VllmProvider NON viene costruito (bootstrap.rs:194) -> comportamento invariato.
--
-- Stessa forma di 0418 (INSERT key,value ON CONFLICT DO NOTHING): idempotente,
-- preserva eventuali override admin. Il gateway/brain le ricaricano a caldo
-- (TtlCache 60s), nessun restart necessario.

INSERT INTO settings (key, value) VALUES
  ('nexus_gateway_url', 'http://127.0.0.1:4060'),
  ('gateway.complete_timeout_seconds', '120'),
  ('gateway.stream_timeout_seconds', '300'),
  ('anthropic_base_url', 'https://api.anthropic.com/v1'),
  ('vllm_base_url', '')
ON CONFLICT (key) DO NOTHING;
