-- Migrazione 0719: saldi interrogabili estesi a openrouter e kimi.
--
-- Il worker di sync del saldo (ex deepseek_balance_sync, ora
-- provider_balance_sync in mcp-core) interroga TRE fornitori con endpoint di
-- saldo: deepseek (/user/balance), openrouter (/credits con ripiego /auth/key)
-- e kimi (/users/me/balance). Il saldo REALE osservato atterra qui, accanto
-- allo spent derivato.
--
-- Tre colonne e non una: il numero senza il QUANDO e il DA DOVE e' un'opinione
-- (regola O/Q). `balance_source` distingue l'endpoint dedicato dal ripiego
-- /auth/key di openrouter (chiavi senza permesso credits): due letture con
-- semantiche diverse non devono essere indistinguibili a posteriori.
ALTER TABLE provider_budget_status
    ADD COLUMN IF NOT EXISTS last_known_balance_usd numeric(12,6) NULL,
    ADD COLUMN IF NOT EXISTS balance_observed_at timestamptz NULL,
    ADD COLUMN IF NOT EXISTS balance_source text NULL
        CHECK (balance_source IS NULL OR balance_source IN ('endpoint','auth_key_fallback'));

COMMENT ON COLUMN provider_budget_status.last_known_balance_usd IS
    'Saldo USD osservato dall''endpoint del fornitore (worker provider_balance_sync). NULL = mai osservato: non si scrive uno 0 di comodo (regola Q).';
COMMENT ON COLUMN provider_budget_status.balance_observed_at IS
    'Istante dell''ultima osservazione del saldo. NULL = mai osservato.';
COMMENT ON COLUMN provider_budget_status.balance_source IS
    'Da dove viene il saldo: endpoint (l''endpoint dedicato del fornitore) oppure auth_key_fallback (openrouter /auth/key quando /credits risponde 403). Identificatori inglesi canonici (regola N).';

-- I due fornitori nuovi con endpoint di saldo. Budget 0 = l'admin imposta il
-- valore reale (stessa convenzione del seed 0173): a budget 0 lo spent
-- derivato resta 0 e is_exhausted resta falso, ma il saldo grezzo osservato
-- e' comunque esposto, che e' il valore del sensore.
INSERT INTO provider_budget_status (provider, monthly_budget_usd, min_threshold_usd) VALUES
    ('openrouter', 0, 0.50),
    ('kimi',       0, 0.50)
ON CONFLICT (provider) DO NOTHING;

-- La vista espone il saldo osservato. Colonne nuove IN CODA: CREATE OR REPLACE
-- accetta solo colonne finali aggiunte, con l'ordine esistente invariato
-- (stesso vincolo della 0478 su v_model_capabilities). Corpo IDENTICO alla
-- 0173, piu' le tre colonne.
CREATE OR REPLACE VIEW provider_budget_remaining_view AS
SELECT
    provider,
    monthly_budget_usd,
    spent_current_period_usd,
    GREATEST(monthly_budget_usd - spent_current_period_usd, 0) AS remaining_usd,
    min_threshold_usd,
    (monthly_budget_usd - spent_current_period_usd) < min_threshold_usd AS is_exhausted,
    period_start,
    updated_at,
    last_known_balance_usd,
    balance_observed_at,
    balance_source
FROM provider_budget_status;

COMMENT ON VIEW provider_budget_remaining_view IS
'View comoda per la UI admin: budget residuo + flag esaurito per ogni provider, piu'' il saldo REALE osservato dagli endpoint dei fornitori (deepseek/openrouter/kimi via provider_balance_sync).';
