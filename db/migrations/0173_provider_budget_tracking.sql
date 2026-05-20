-- Migrazione 0173: tracking budget per provider AI.
--
-- I provider AI consumer (Anthropic, OpenAI, Google, Mistral) NON espongono
-- un endpoint pubblico per leggere il balance/budget residuo via API key.
-- Solo DeepSeek ha `GET /user/balance`. Quindi:
--   - per i 4 provider senza endpoint: tracciamo INTERNAMENTE il budget,
--     incrementando `spent_current_period_usd` a ogni run completato in
--     base al `total_cost` calcolato (gia' presente in agent_runs.total_cost).
--   - per DeepSeek: il worker `provider_budget_probe` (futuro) puo'
--     sincronizzare con l'endpoint reale ogni N minuti.
--
-- L'admin imposta `monthly_budget_usd` (lo conosce perche' e' lui che ha
-- ricaricato l'account presso il provider). Quando il run reale viene
-- contabilizzato, decrementa il budget residuo. Quando residuo <
-- `min_threshold_usd` (default 1 USD), il provider viene marcato unhealthy
-- con error_kind='budget_exhausted'.
--
-- Quando l'admin ricarica il provider:
--   1. UPDATE provider_budget_status SET monthly_budget_usd = NEW_VALUE
--      (oppure click "ricarica" che resetta a budget originale)
--   2. POST /api/admin/providers/:name/recharge → reset spent_current_period_usd
--      e aggiorna period_start a now()

CREATE TABLE IF NOT EXISTS provider_budget_status (
    provider TEXT PRIMARY KEY,
    monthly_budget_usd NUMERIC(12, 4) NOT NULL DEFAULT 0,
    spent_current_period_usd NUMERIC(12, 6) NOT NULL DEFAULT 0,
    period_start TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    min_threshold_usd NUMERIC(12, 4) NOT NULL DEFAULT 1.0,
    notes TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE provider_budget_status IS
'Tracking budget per provider AI. monthly_budget_usd impostato manualmente dall admin (i provider non espongono balance via API), spent_current_period_usd incrementato da chat_messages.rs::run_completed. Probe marca unhealthy quando (budget - spent) < min_threshold.';

-- Seed iniziale a budget 0 (admin deve impostare il valore reale).
INSERT INTO provider_budget_status (provider, monthly_budget_usd, min_threshold_usd) VALUES
    ('anthropic', 0, 1.0),
    ('openai',    0, 1.0),
    ('google',    0, 1.0),
    ('mistral',   0, 1.0),
    ('deepseek',  0, 0.50)
ON CONFLICT (provider) DO NOTHING;

-- Vista helper: budget residuo per provider + flag esaurito.
CREATE OR REPLACE VIEW provider_budget_remaining_view AS
SELECT
    provider,
    monthly_budget_usd,
    spent_current_period_usd,
    GREATEST(monthly_budget_usd - spent_current_period_usd, 0) AS remaining_usd,
    min_threshold_usd,
    (monthly_budget_usd - spent_current_period_usd) < min_threshold_usd AS is_exhausted,
    period_start,
    updated_at
FROM provider_budget_status;

COMMENT ON VIEW provider_budget_remaining_view IS
'View comoda per la UI admin: budget residuo + flag esaurito per ogni provider.';
