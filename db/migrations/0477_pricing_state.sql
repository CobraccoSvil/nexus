-- 0477_pricing_state.sql
--
-- CAUSA RADICE (regola H): nel catalog ai_price_catalog un costo 0 NON significa
-- "modello gratuito" ma "prezzo PLACEHOLDER non ancora raffinato". catalog_sync
-- (model_catalog_sync.rs ~riga 725) inserisce ogni nuovo modello scoperto via API
-- con input_cost=0/output_cost=0/is_enabled=false: tutti i ~150 modelli a costo 0
-- sono quindi disabilitati. Due conseguenze:
--   (1) non si distingue "prezzo ignoto" (placeholder) da "gratuito reale";
--   (2) cost_score (routing_matrix_auto_promoter.rs) FILTRA input_cost>0.0 nel
--       calcolo min/max della normalizzazione -> un modello davvero gratuito
--       (costo 0) darebbe normalized<0 e score fuori range, quindi abilitare un
--       free romperebbe lo scoring.
--
-- FIX strutturale: colonna esplicita pricing_state che separa i tre stati. Niente
-- piu' overload semantico del valore 0 sui costi. Lo scoring puo' trattare 'free'
-- come costo reale 0 (ottimo per cost_direction='asc') e 'unknown' come neutro,
-- senza ambiguita'.
--
-- Idempotente.

-- a. Colonna esplicita. Valori ammessi:
--    'unknown' = prezzo placeholder/ignoto (cost 0 non raffinato dal discovery)
--    'priced'  = prezzo reale > 0
--    'free'    = gratuito reale confermato (promosso A MANO da admin/seed, MAI auto)
ALTER TABLE ai_price_catalog
  ADD COLUMN IF NOT EXISTS pricing_state TEXT NOT NULL DEFAULT 'unknown';

-- CHECK separato + idempotente (ADD COLUMN IF NOT EXISTS non supporta CHECK inline
-- ri-eseguibile in modo pulito su colonna gia' esistente).
DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint WHERE conname = 'chk_price_catalog_pricing_state'
  ) THEN
    ALTER TABLE ai_price_catalog
      ADD CONSTRAINT chk_price_catalog_pricing_state
      CHECK (pricing_state IN ('unknown', 'priced', 'free'));
  END IF;
END $$;

-- b. Backfill: tutto cio' che ha gia' un prezzo reale > 0 e' 'priced'. Gli altri
--    restano 'unknown' (default). NESSUNA promozione automatica a 'free'.
UPDATE ai_price_catalog
SET pricing_state = 'priced'
WHERE input_cost_per_million_tokens > 0
   OR output_cost_per_million_tokens > 0;

-- c. Indice di supporto (cost_score / filtri admin per stato di pricing).
CREATE INDEX IF NOT EXISTS idx_price_catalog_pricing_state
  ON ai_price_catalog (pricing_state);

-- d. Policy: rendi SCOPRIBILI i modelli 'gemma' dal catalog_sync, senza forzarne
--    l'abilitazione (il probe-on-insert decide se passano). Stato post-0472 per
--    google:
--      allowed = {'^gemini-'}                            -> NON matcha 'gemma-...'
--      denied  = {'embedding','image',...,'gemma',...}   -> 'gemma' (substring) li nega
--    Fix: togli 'gemma' dalla denylist e aggiungi '^gemma' all'allowlist (anchor,
--    cosi' non riapre per errore 'gemini' o altre famiglie). Array finali espliciti
--    per leggibilita' e determinismo (niente array_remove/unnest con ordine ambiguo).
UPDATE nexus_model_selection_policy SET
    allowed_patterns = ARRAY['^gemini-','^gemma'],
    denied_patterns  = ARRAY['embedding','image','imagen','tts','audio','live','robotics','computer-use','aqa','^gemini-1','nano-banana'],
    updated_at       = now()
WHERE provider = 'google';
