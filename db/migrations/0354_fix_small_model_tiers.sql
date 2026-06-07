-- 0354_fix_small_model_tiers.sql
--
-- Corregge il performance_tier dei modelli "piccoli" classificati erroneamente
-- come 'medium'.
--
-- CAUSA: ai_price_catalog.performance_tier ha DEFAULT 'medium' e il catalog_sync
-- inseriva i nuovi modelli scoperti via API senza tier -> ogni modello piccolo
-- (ministral-3b/8b, mistral-small-*, magistral-small, nemo) diventava 'medium'
-- ed entrava nel pool dei candidati per gli intent agentici 'medium',
-- degradando la qualita' (es. ministral-8b proposto per agentic_default).
--
-- FIX in due parti:
--   - Questa migrazione: riclassifica gli esistenti a 'light'.
--   - Codice: infer_tier_from_name() nel catalog_sync assegna il tier dal nome
--     ai NUOVI insert (non piu' il default 'medium'). Le due regole sono
--     allineate. Cosi' il problema non si ripresenta.
--
-- Scope: solo Mistral (gli altri provider erano gia' classificati dal seed
-- 0032). Idempotente. NON tocca righe con override manuale esplicito (se mai
-- introdotto): qui usiamo solo il match per nome, sicuro.

UPDATE ai_price_catalog
   SET performance_tier = 'light',
       updated_at = NOW()
 WHERE provider = 'mistral'
   AND performance_tier = 'medium'
   AND (
        model LIKE 'ministral-%'   -- ministral-3b / 8b / 14b: famiglia piccola
     OR model LIKE '%small%'       -- mistral-small-*, magistral-small-*
     OR model LIKE '%nemo%'        -- open-mistral-nemo*
   );
