-- Migrazione 0172: model-level health history e contatore fallimenti.
--
-- La migrazione 0097 ha introdotto `nexus_provider_health_history` che traccia
-- la salute *del provider nel suo complesso* (probato con UN solo modello di
-- default). Questo non basta quando il provider risponde ma un singolo modello
-- e' broken: es. DeepSeek API risponde, ma `deepseek-v3` e' stato dismesso e
-- restituisce errore; oppure Google API risponde, ma `gemini-3.5-flash` non e'
-- ancora rilasciato e produce hollow_completion.
--
-- Questa migrazione introduce:
--   1. Tabella `ai_model_health_history` (append-only) parallela a quella
--      provider-level, ma con granularita' (provider, model).
--   2. Colonna `consecutive_failures` su `ai_price_catalog` come contatore
--      per la logica auto-disable: dopo N fallimenti consecutivi (non
--      provider-wide come quota/billing) il modello viene disattivato
--      automaticamente; al primo successo il contatore torna a zero e la
--      flag `is_enabled` viene riportata a TRUE.
--
-- Il worker che popola tutto questo si chiama `model_health_probe` (vedi
-- `crates/mcp-core/src/model_health_probe.rs`).

CREATE TABLE IF NOT EXISTS ai_model_health_history (
    id BIGSERIAL PRIMARY KEY,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    healthy BOOLEAN NOT NULL,
    latency_ms INT,
    -- Categoria errore quando healthy=FALSE. Riusa la nomenclatura di
    -- `nexus_provider_health_history` piu' due categorie specifiche del
    -- livello modello: "model_not_found", "hollow_completion".
    error_kind TEXT,
    error_message TEXT,
    checked_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- "Ultimo check per modello" e' la query piu' frequente (UI, dashboard).
CREATE INDEX IF NOT EXISTS idx_ai_model_health_provider_model_checked
    ON ai_model_health_history (provider, model, checked_at DESC);

-- Filtro per error_kind quando l'admin vuole capire perche' un modello
-- e' stato auto-disabilitato.
CREATE INDEX IF NOT EXISTS idx_ai_model_health_error_kind
    ON ai_model_health_history (error_kind, checked_at DESC)
    WHERE error_kind IS NOT NULL;

COMMENT ON TABLE ai_model_health_history IS
'Storico health check per singolo modello AI. Popolato dal worker model_health_probe (default cadenza 30m). Append-only.';

-- Contatore fallimenti consecutivi per auto-disable.
-- Inizia a 0 per tutti i modelli esistenti. Il worker incrementa su
-- fallimento "model-specific" (non quota/billing che disabilita il provider
-- intero); azzera su successo.
ALTER TABLE ai_price_catalog
    ADD COLUMN IF NOT EXISTS consecutive_failures INT NOT NULL DEFAULT 0;

ALTER TABLE ai_price_catalog
    ADD COLUMN IF NOT EXISTS auto_disabled_at TIMESTAMPTZ;

ALTER TABLE ai_price_catalog
    ADD COLUMN IF NOT EXISTS auto_disabled_reason TEXT;

COMMENT ON COLUMN ai_price_catalog.consecutive_failures IS
'Fallimenti consecutivi del modello al probe (errori model-specific). Reset a 0 al primo successo.';
COMMENT ON COLUMN ai_price_catalog.auto_disabled_at IS
'Timestamp dell auto-disable (NULL se enabled manualmente o mai auto-disabled).';
COMMENT ON COLUMN ai_price_catalog.auto_disabled_reason IS
'Motivo dell auto-disable: error_kind dell ultimo fallimento (es. model_not_found, hollow_completion).';

-- Settings per il worker. Default: probe disabilitato per opt-in, intervallo
-- 30 minuti, soglia 3 fallimenti consecutivi prima di auto-disable.
INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('model_health_probe_enabled', 'true', 'ai',
     'Abilita il worker model_health_probe che pinga ogni modello enabled in catalog.', false),
    ('model_health_probe_interval_s', '1800', 'ai',
     'Intervallo in secondi tra cicli di probe (default 30 min, minimo 300).', false),
    ('model_health_probe_failure_threshold', '3', 'ai',
     'Numero di fallimenti consecutivi (model-specific) prima dell auto-disable.', false),
    ('model_catalog_sync_enabled', 'true', 'ai',
     'Abilita il worker periodico che chiama run_catalog_sync (sync da LiteLLM GitHub).', false),
    ('model_catalog_sync_interval_s', '43200', 'ai',
     'Intervallo in secondi tra sync catalog (default 12h, minimo 3600).', false)
ON CONFLICT (key) DO NOTHING;
