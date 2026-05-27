-- Bug 7 (audit 26/05/2026): worker auto-sync ai_price_catalog dai provider.
-- Quando un provider deprecava un modello (es. DeepSeek v3 -> v4) il catalog
-- restava stale per settimane e i run AI fallivano con "hollow completion"
-- perche' chiamavano modelli inesistenti lato provider.
--
-- Il worker `model_catalog_sync` in mcp-core (mig 0182):
--   1. ogni N ore chiama GET /v1/models di ogni provider con api_key configurato
--   2. INSERT nuovi modelli con is_enabled=false (admin deve verificare prezzi)
--   3. UPDATE is_enabled=false per modelli non piu' esposti dall'API
--   4. Storico ogni delta in ai_price_catalog_audit
--   5. Emit notification dispatcher per admin sui cambi rilevati

INSERT INTO settings (key, value, category, description) VALUES
  ('catalog_sync.enabled', 'true', 'agent',
    'Attiva/disattiva il worker periodico di sync catalog modelli dai provider.'),
  ('catalog_sync.interval_hours', '6', 'agent',
    'Intervallo (ore) tra i tick del worker. Default 6 = 4 sync al giorno.'),
  ('catalog_sync.providers', 'anthropic,openai,mistral,deepseek', 'agent',
    'Lista CSV provider da sincronizzare. Esclusi google (richiede SDK Vertex specifico) e provider locali.'),
  ('catalog_sync.disable_missing', 'true', 'agent',
    'Se TRUE, disabilita i modelli del catalog non piu esposti dall API. Se FALSE solo log.'),
  ('catalog_sync.insert_new_disabled', 'true', 'agent',
    'Se TRUE, modelli nuovi vengono inseriti con is_enabled=false (admin verifica prezzi prima di abilitare).')
ON CONFLICT (key) DO UPDATE SET
  value = EXCLUDED.value, description = EXCLUDED.description, category = EXCLUDED.category, updated_at = NOW();

-- Tabella audit per tracciare i delta rilevati dal worker.
CREATE TABLE IF NOT EXISTS ai_price_catalog_audit (
  id           UUID NOT NULL DEFAULT gen_random_uuid() PRIMARY KEY,
  occurred_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  provider     TEXT NOT NULL,
  model        TEXT NOT NULL,
  -- 'inserted' = nuovo modello aggiunto dall API
  -- 'disabled' = modello non piu esposto dall API (probabile deprecazione)
  -- 'reenabled' = modello rilevato di nuovo dall API dopo essere stato disabled
  action       TEXT NOT NULL CHECK (action IN ('inserted','disabled','reenabled')),
  details      JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE INDEX IF NOT EXISTS idx_catalog_audit_recent ON ai_price_catalog_audit (occurred_at DESC);
CREATE INDEX IF NOT EXISTS idx_catalog_audit_provider ON ai_price_catalog_audit (provider, occurred_at DESC);
