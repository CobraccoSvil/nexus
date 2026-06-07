-- 0359_backfill_catalog_capabilities.sql
--
-- Popola retroattivamente le capabilities VUOTE dei modelli gia' abilitati nel
-- catalog. Causa radice del "Mistral usa sempre small".
--
-- DIAGNOSI (verificata sul DB + codice):
--   infer_capabilities_from_name() (catalog_sync) inferisce correttamente le
--   capabilities dal nome (es. mistral-large -> [reasoning,code,chat]), MA il
--   sync le applica SOLO al probe-on-insert: l'UPDATE ha
--   `WHERE ... is_enabled = false`. I modelli gia' abilitati con capabilities=[]
--   (mistral-large-latest, mistral-large-2512, tutti i mistral-medium-*, e vari
--   altri provider) non vengono MAI aggiornati e restano [].
--
--   Conseguenza: il routing agentico filtra per capability matching
--   (capabilities @> [...]). Un modello con caps [] non matcha NULLA, quindi e'
--   invisibile alla selezione. Tra i Mistral, SOLO mistral-small-latest ha
--   capabilities popolate (["code","chat","fix"]): risultato, ogni selezione
--   Mistral con capability filter sceglie small, mai large/medium. Questo
--   spiega perche' i fix su routing_matrix (0353) e slot-matrix (0357) non
--   bastavano: il problema era nel catalog, a monte di ogni selettore.
--
-- FIX (regola H, causa radice):
--   Backfill delle capabilities vuote replicando le regole di
--   infer_capabilities_from_name (stessa euristica per famiglia). Idempotente:
--   tocca SOLO le righe con capabilities NULL o '[]' (rispetta override manuali
--   e righe gia' popolate). Sopravvive a wipe+migrate; per i NUOVI modelli il
--   probe-on-insert continua a popolare.
--
-- Limite noto (documentato): il sync popola le caps solo all'enable. Un modello
--   abilitato manualmente con caps vuote resterebbe []. Coperto da questa
--   migrazione per gli esistenti; per il futuro vale il probe-on-insert.

-- ── MISTRAL (l'ordine replica infer: codestral/devstral, poi large/magistral,
--    poi medium, poi resto) ────────────────────────────────────────────────
UPDATE ai_price_catalog
   SET capabilities = CASE
        WHEN model ILIKE '%codestral%' OR model ILIKE '%devstral%'
            THEN '["code","chat"]'::jsonb
        WHEN model ILIKE '%large%' OR model ILIKE '%magistral%'
            THEN '["reasoning","code","chat"]'::jsonb
        WHEN model ILIKE '%medium%'
            THEN '["code","chat"]'::jsonb
        ELSE '["chat"]'::jsonb
       END,
       updated_at = NOW()
 WHERE provider = 'mistral'
   AND (capabilities IS NULL OR capabilities = '[]'::jsonb);

-- ── GOOGLE ──────────────────────────────────────────────────────────────────
UPDATE ai_price_catalog
   SET capabilities = CASE
        WHEN model ILIKE '%pro%'        THEN '["reasoning","code","long-context","chat"]'::jsonb
        WHEN model ILIKE '%flash-lite%' THEN '["chat","simple"]'::jsonb
        WHEN model ILIKE '%flash%'      THEN '["code","chat","fix"]'::jsonb
        ELSE '["chat"]'::jsonb
       END,
       updated_at = NOW()
 WHERE provider = 'google'
   AND (capabilities IS NULL OR capabilities = '[]'::jsonb);

-- ── ANTHROPIC ───────────────────────────────────────────────────────────────
UPDATE ai_price_catalog
   SET capabilities = CASE
        WHEN model ILIKE '%opus%' OR model ILIKE '%sonnet%'
            THEN '["reasoning","code","long-context","chat"]'::jsonb
        WHEN model ILIKE '%haiku%' THEN '["chat","simple"]'::jsonb
        ELSE '["chat"]'::jsonb
       END,
       updated_at = NOW()
 WHERE provider = 'anthropic'
   AND (capabilities IS NULL OR capabilities = '[]'::jsonb);

-- ── OPENAI ──────────────────────────────────────────────────────────────────
UPDATE ai_price_catalog
   SET capabilities = CASE
        WHEN model ILIKE '%codex%'                       THEN '["code","chat"]'::jsonb
        WHEN model ILIKE '%nano%' OR model ILIKE '%mini%' THEN '["chat","simple"]'::jsonb
        ELSE '["reasoning","code","chat"]'::jsonb
       END,
       updated_at = NOW()
 WHERE provider = 'openai'
   AND (capabilities IS NULL OR capabilities = '[]'::jsonb);

-- ── DEEPSEEK ────────────────────────────────────────────────────────────────
UPDATE ai_price_catalog
   SET capabilities = CASE
        WHEN model ILIKE '%pro%' OR model ILIKE '%reasoner%'
            THEN '["reasoning","code","chat"]'::jsonb
        ELSE '["code","chat"]'::jsonb
       END,
       updated_at = NOW()
 WHERE provider = 'deepseek'
   AND (capabilities IS NULL OR capabilities = '[]'::jsonb);
