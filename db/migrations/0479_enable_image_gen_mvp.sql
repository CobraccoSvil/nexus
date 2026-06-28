-- 0479_enable_image_gen_mvp.sql  (PR6b-2 — abilitazione modelli image-gen MVP)
--
-- CONTESTO: PR6a (mig 0478) ha aggiunto la capability supports_image_gen + il
-- purpose interno 'generate_image' (tier=light, required_capability=image_gen,
-- requires_tool_use=false). PR6b-1 ha aggiunto al gateway l'endpoint
-- POST /v1/images/generations con due backend di generazione REALI:
--   - OpenAI  -> POST /images/generations  (modelli gpt-image-*, dall-e-*)
--   - Vertex  -> Imagen :predict           (modelli imagen-*)
-- PR6b-2 ha aggiunto il tool nexus_generate_image (risolve il purpose -> gateway
-- -> salva l'immagine path-safe nel progetto).
--
-- PROBLEMA: nessun modello image-gen e' is_enabled=true, quindi
-- best_model_for_tier(tier='light', capability='image_gen') NON ha candidati e
-- il purpose 'generate_image' non risolve (il tool ritorna sempre "nessun
-- modello capable"). Inoltre quasi tutti gli Imagen/gpt-image sono classificati
-- performance_tier='medium', mentre il purpose e' tier='light': il resolver
-- tier-only NON-agentico interroga ESATTAMENTE il tier 'light' (best_model_for_tier
-- non degrada il tier per i purpose non-agentici), quindi un modello 'medium'
-- non verrebbe mai scelto da questo purpose.
--
-- SOLUZIONE (fix definitivo, regola H — non una toppa):
--   1. Abilita SOLO i modelli image-gen REALMENTE chiamabili dagli endpoint di
--      PR6b-1 (OpenAI /images/generations e Vertex Imagen :predict).
--   2. Allinea il loro performance_tier a 'light' cosi' coincide col tier del
--      purpose 'generate_image' (mig 0478). Senza questo allineamento il purpose
--      light non li selezionerebbe mai.
--   3. Marca capability_source='manual': cosi' il catalog_sync NON tocca piu'
--      ne' supports_image_gen ne' performance_tier ne' is_enabled di queste
--      righe (il riallineamento del sync agisce SOLO su capability_source='auto',
--      vedi model_catalog_sync.rs:1138). La scelta sopravvive a sync/deploy/wipe
--      + re-apply migrazioni (regola H punto 2/5).
--
-- ESCLUSIONI VOLUTE:
--   - gemini-2.5-flash-image / gemini-*-flash-image*: generano via
--     generateContent, NON via :predict -> NON gestiti dall'endpoint di PR6b-1.
--     Abilitarli ora porterebbe a una chiamata che fallisce sul dialetto sbagliato.
--   - automl-vision-*: sono modelli di classificazione/object-detection, non
--     image-generation (falsi positivi del backfill per-nome della mig 0478).
--
-- NOTA SULLA SALUTE REALE: questa migrazione rende i modelli SELEZIONABILI dal
-- routing; NON garantisce che il provider risponda. Se la API key OpenAI non ha
-- accesso a gpt-image-* o il progetto GCP non ha Imagen abilitato, la chiamata
-- fallira' ESPLICITAMENTE (HTTP 4xx/5xx dal gateway risale al tool, regola H:
-- errore onesto, niente fallback inventato). E' il comportamento atteso: l'admin
-- abilita un solo provider image-gen funzionante e gli altri restano come
-- alternativa pronta.
--
-- Idempotente.

-- 1+2+3. OpenAI /images/generations: gpt-image-1-mini (gia' tier light, il piu'
--        economico) + gpt-image-1 (riallineato a light). Solo se la riga esiste.
UPDATE ai_price_catalog
SET is_enabled         = true,
    supports_image_gen = true,
    performance_tier   = 'light',
    capability_source  = 'manual',
    updated_at         = NOW()
WHERE provider = 'openai'
  AND model IN ('gpt-image-1-mini', 'gpt-image-1');

-- 1+2+3. Vertex Imagen :predict: imagen-4.0 standard + fast (riallineati a light).
UPDATE ai_price_catalog
SET is_enabled         = true,
    supports_image_gen = true,
    performance_tier   = 'light',
    capability_source  = 'manual',
    updated_at         = NOW()
WHERE provider = 'google'
  AND model IN ('imagen-4.0-generate-001', 'imagen-4.0-fast-generate-001');

-- Verifica (no-op informativa): conta i candidati instradabili dal purpose
-- 'generate_image' (tier=light, capability=image_gen). Deve essere >= 1.
SELECT 'image_gen_light_candidates' AS scope,
       provider,
       COUNT(*) AS count
FROM ai_price_catalog
WHERE supports_image_gen = true
  AND is_enabled = true
  AND performance_tier = 'light'
GROUP BY provider
ORDER BY provider;
