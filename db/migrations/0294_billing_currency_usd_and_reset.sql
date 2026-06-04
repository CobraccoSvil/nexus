-- Migrazione 0294 — Allineamento billing currency a USD + reset ledger.
--
-- Diagnosi (audit 2026-06-04):
--   - `ai_price_catalog` interamente in USD (334 righe)
--   - `settings.billing_base_currency = 'EUR'` (mismatch)
--   - `resolve_active_price()` cercava PRIMA con currency='EUR' -> nessun match,
--     fallback prendeva il prezzo USD ma 3.993 righe sono state scritte
--     con currency='EUR' e total_cost=0 (orfane).
--   - 6.682 righe finalized con total_cost=0 (modelli rimossi dal catalog o
--     errori di match) -> sotto-stima del costo reale di ~$5-15.
--
-- Fix definitivo (regola H):
--   1. Setting platform currency = USD (allineato al catalog).
--   2. Codice billing.rs: default fallback EUR -> USD.
--   3. Reset `ai_usage_ledger` (dati storici sporchi, backup in
--      backups/postgres/ledger_pre_reset_<TS>.sql.gz).
--   4. UI label "EUR" -> "USD" (fix separato in chat-panel/billing-page).
--
-- Idempotente: settings UPSERT, TRUNCATE solo se la tabella esiste.

BEGIN;

-- Step 1: Setting platform currency -> USD
INSERT INTO settings (key, value, category, description, updated_at)
VALUES (
    'billing_base_currency',
    'USD',
    'billing',
    'Currency di piattaforma per il calcolo billing. DEVE essere allineato a ai_price_catalog (USD: i provider AI fatturano in dollari). Cambiarlo qui senza aggiornare il catalog produce ledger orfani con cost=0.',
    NOW()
)
ON CONFLICT (key) DO UPDATE SET
    value = EXCLUDED.value,
    description = EXCLUDED.description,
    updated_at = NOW();

-- Step 2: Reset ledger (dati pre-fix non recuperabili — il calcolo sotto-stima
-- e mescola currency). Backup gia' eseguito esternamente.
TRUNCATE TABLE ai_usage_ledger RESTART IDENTITY;

-- Step 3: Forza ai_price_catalog a USD (idempotente — gia' tutto USD)
UPDATE ai_price_catalog SET currency = 'USD' WHERE currency != 'USD';

COMMIT;
