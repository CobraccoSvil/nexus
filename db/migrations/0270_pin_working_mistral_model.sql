-- 0270: usa il modello Mistral PINNED che funziona davvero con l'account
-- configurato, non l'alias rotto.
--
-- Root cause: l'alias `mistral-large-latest` (e `mistral-large-2512`) risolve
-- lato Mistral a `labs-leanstral-2603`, un modello "labs" che questo account
-- NON puo' usare (HTTP 403 "Model labs ... not enabled"). Nel catalog questi
-- alias risultavano is_enabled=true (15/14 consecutive_failures ma non
-- auto-disabilitati: bug del probe, vedi follow-up), mentre `mistral-large-2411`
-- - che FUNZIONA davvero (successi reali con finish_reason=tool_calls nei log) -
-- era is_enabled=false. Risultato: il planner sceglieva l'alias rotto -> 403 ->
-- nessun piano -> run fallito.
--
-- Fix (config corretta, allineata alla realta' dell'account):
--   1. Catalog: abilita mistral-large-2411 (funzionante), disabilita gli alias
--      che danno 403 (mistral-large-latest, mistral-large-2512).
--   2. Planner: usa mistral-large-2411.
--   3. Routing matrix: ogni riga con mistral-large-latest -> mistral-large-2411.
-- Idempotente.

-- 1. Catalog: allinea is_enabled alla realta'.
UPDATE ai_price_catalog
SET is_enabled = true, consecutive_failures = 0, consecutive_tool_failures = 0,
    auto_disabled_at = NULL, auto_disabled_reason = NULL, updated_at = NOW()
WHERE provider = 'mistral' AND model = 'mistral-large-2411';

UPDATE ai_price_catalog
SET is_enabled = false, auto_disabled_reason = '0270: alias risolve a labs-leanstral-2603 (403 non abilitato per account)', updated_at = NOW()
WHERE provider = 'mistral' AND model IN ('mistral-large-latest', 'mistral-large-2512');

-- 2. Planner -> modello pinned funzionante.
UPDATE nexus_purpose_model
SET model_id = 'mistral-large-2411',
    notes = 'planner: mistral pinned funzionante (alias -latest risolve a labs 403, mig 0270)',
    updated_at = NOW()
WHERE purpose = 'planner' AND provider = 'mistral';

-- 3. Routing matrix: sostituisci l'alias rotto col pinned ovunque.
UPDATE nexus_routing_matrix
SET model_id = 'mistral-large-2411', updated_at = NOW()
WHERE provider = 'mistral' AND model_id = 'mistral-large-latest';
