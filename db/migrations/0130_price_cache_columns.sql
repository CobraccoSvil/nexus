-- Migrazione 0130: aggiunge pricing cache ad ai_price_catalog
-- e popola i valori per Anthropic (unico provider con cache differenziata).
-- Risolve gap sezione 8d / fix 8.4 del piano.

ALTER TABLE ai_price_catalog
    ADD COLUMN IF NOT EXISTS cache_read_cost_per_million_tokens     NUMERIC(18,6),
    ADD COLUMN IF NOT EXISTS cache_creation_cost_per_million_tokens NUMERIC(18,6);

COMMENT ON COLUMN ai_price_catalog.cache_read_cost_per_million_tokens
    IS 'Costo per milione di token LETTI da cache (es. Anthropic: 0.1x input_cost). NULL se provider non supporta caching.';
COMMENT ON COLUMN ai_price_catalog.cache_creation_cost_per_million_tokens
    IS 'Costo per milione di token SCRITTI in cache (es. Anthropic: 1.25x input_cost). NULL se provider non supporta caching.';

-- Popola pricing cache per Anthropic: cache_read = 0.1x, cache_creation = 1.25x input
UPDATE ai_price_catalog
SET
    cache_read_cost_per_million_tokens     = ROUND(input_cost_per_million_tokens * 0.10, 6),
    cache_creation_cost_per_million_tokens = ROUND(input_cost_per_million_tokens * 1.25, 6)
WHERE provider = 'anthropic';
