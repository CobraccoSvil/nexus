-- 0583_pricing_state_backfill_trigger.sql
--
-- CAUSA RADICE (diagnosi 2026-07-13): i 9 modelli di groq/openrouter/perplexity
-- hanno pricing_state='unknown' NONOSTANTE input/output_cost > 0 reali. Le mig
-- 0566-0568 inseriscono in ai_price_catalog SENZA la colonna pricing_state, che e'
-- NOT NULL DEFAULT 'unknown' (mig 0477): nascono 'unknown'. Il backfill one-shot di
-- 0477 (WHERE cost>0) e' girato una volta sola all'apply, quando i 3 provider non
-- esistevano; e nessun path automatico riconcilia unknown->priced (l'UPSERT del sync
-- LiteLLM aggiorna i costi ma NON tocca pricing_state; la derivazione cost>0->'priced'
-- vive solo negli endpoint admin manuali di billing.rs).
--
-- pricing_state NON esclude a runtime (nessun selettore lo filtra; verificato
-- select_models_tierchain model_selection.rs), ma sporca il cost_score dell'auto-promoter
-- (unknown -> pool min/max) ed e' un landmine architetturale (ogni nuova migrazione/
-- discovery che dimentica la colonna ricade nello stesso bug).
--
-- FIX DEFINITIVO (regola H causa radice + regola L punto unico):
--  (1) backfill idempotente delle righe rimaste 'unknown' con costo reale > 0;
--  (2) trigger BEFORE INSERT/UPDATE che deriva pricing_state da costo > 0 come PUNTO
--      UNICO: nessun call site (migrazioni future, discovery, sync) puo' piu'
--      dimenticarlo. Promuove SOLO 'unknown'->'priced' (non degrada 'priced', non
--      tocca 'free' impostato esplicitamente da admin/seed).

-- (1) Backfill idempotente
UPDATE ai_price_catalog
SET pricing_state = 'priced'
WHERE pricing_state = 'unknown'
  AND (input_cost_per_million_tokens > 0 OR output_cost_per_million_tokens > 0);

-- (2) Trigger punto unico: deriva pricing_state dal costo alla FONTE della scrittura.
CREATE OR REPLACE FUNCTION ai_price_catalog_derive_pricing_state()
RETURNS TRIGGER AS $$
BEGIN
  IF NEW.pricing_state = 'unknown'
     AND (COALESCE(NEW.input_cost_per_million_tokens, 0) > 0
          OR COALESCE(NEW.output_cost_per_million_tokens, 0) > 0) THEN
    NEW.pricing_state := 'priced';
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_ai_price_catalog_pricing_state ON ai_price_catalog;
CREATE TRIGGER trg_ai_price_catalog_pricing_state
  BEFORE INSERT OR UPDATE ON ai_price_catalog
  FOR EACH ROW
  EXECUTE FUNCTION ai_price_catalog_derive_pricing_state();
