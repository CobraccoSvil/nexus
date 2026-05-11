-- Migrazione 0129: aggiunge colonne cache token ad ai_usage_ledger
-- Risolve gap G4 / sezione 8b-causa#2: _record_usage ignorava
-- cache_read_input_tokens e cache_creation_input_tokens di Anthropic.
-- Le nuove colonne permettono di calcolare il costo reale (cache_read = 0.1x,
-- cache_creation = 1.25x) e misurare il cache hit rate via query SQL.

ALTER TABLE ai_usage_ledger
    ADD COLUMN IF NOT EXISTS cache_read_tokens      BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS cache_creation_tokens  BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS cache_read_cost        NUMERIC(18,6)  NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS cache_creation_cost    NUMERIC(18,6)  NOT NULL DEFAULT 0;

COMMENT ON COLUMN ai_usage_ledger.cache_read_tokens     IS 'Token letti da cache Anthropic (cache_read_input_tokens). Costo = 0.1x input_cost.';
COMMENT ON COLUMN ai_usage_ledger.cache_creation_tokens IS 'Token scritti in cache Anthropic (cache_creation_input_tokens). Costo = 1.25x input_cost.';
COMMENT ON COLUMN ai_usage_ledger.cache_read_cost       IS 'Costo effettivo dei token letti da cache (gia'' incluso in total_cost).';
COMMENT ON COLUMN ai_usage_ledger.cache_creation_cost   IS 'Costo effettivo dei token scritti in cache (gia'' incluso in total_cost).';

-- Indice per query "cache hit rate" rapide per provider
CREATE INDEX IF NOT EXISTS idx_ledger_cache_provider_time
    ON ai_usage_ledger (provider, created_at DESC)
    WHERE cache_read_tokens > 0;
