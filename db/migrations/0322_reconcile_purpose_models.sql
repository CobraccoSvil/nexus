-- 0322_reconcile_purpose_models.sql  (ADR 0025)
--
-- Fix del "completamento vuoto a cascata" (run reale 99a87271): un turno risolveva
-- su gemini-2.5-pro (thinking) -> hollow -> cascade -> deepseek-v4-pro hollow ->
-- "Nessuna risposta utile". Due cause radice residue:
--
--  A) La mig 0319 ha usato un pattern troppo largo (gemini-2.5%) marcando ANCHE
--     gemini-2.5-flash / flash-lite come 'disable_for_tools' (sono NON-reasoning).
--     Il classificatore li classifica gia' come 'none'; qui correggiamo i dati.
--  B) nexus_purpose_model non era stato riconciliato (le mig 0321 toccavano solo
--     default_model + settings): puntava a modelli DISABILITATI (planner ->
--     mistral-large-2411, agent_tier_haiku -> deepseek-chat) e a thinking model
--     per il fallback (loop_fallback_default -> gemini-2.5-pro) che genera hollow.
--
-- Idempotente. Riconciliazione deterministica sul miglior modello ENABLED
-- NON-thinking (agentic_thinking_policy='none') tool-capable.

-- A) gemini-2.5-flash / flash-lite NON sono reasoning -> policy 'none'
--    (gemini-2.5-pro resta 'disable_for_tools'). Solo righe 'auto' o gia' sbagliate.
UPDATE ai_price_catalog
SET agentic_thinking_policy = 'none', updated_at = NOW()
WHERE provider = 'google'
  AND model LIKE 'gemini-2.5-flash%'
  AND agentic_thinking_policy = 'disable_for_tools';

-- B1) purpose con modello DISABILITATO -> miglior enabled non-thinking dello STESSO provider.
WITH best_prov AS (
    SELECT provider, model FROM (
        SELECT provider, model,
               row_number() OVER (PARTITION BY provider
                   ORDER BY is_featured DESC, input_cost_per_million_tokens ASC NULLS LAST) AS rn
        FROM ai_price_catalog
        WHERE is_enabled = true AND supports_tool_use = true AND agentic_thinking_policy = 'none'
    ) t WHERE rn = 1
)
UPDATE nexus_purpose_model pm
SET model_id = b.model, updated_at = NOW()
FROM best_prov b
WHERE b.provider = pm.provider
  AND NOT EXISTS (
      SELECT 1 FROM ai_price_catalog c
      WHERE c.provider = pm.provider AND c.model = pm.model_id AND c.is_enabled = true
  );

-- B2) purpose ancora rotti (provider senza enabled non-thinking) -> miglior enabled GLOBALE.
WITH best_global AS (
    SELECT provider, model FROM ai_price_catalog
    WHERE is_enabled = true AND supports_tool_use = true AND agentic_thinking_policy = 'none'
    ORDER BY is_featured DESC, input_cost_per_million_tokens ASC NULLS LAST
    LIMIT 1
)
UPDATE nexus_purpose_model pm
SET provider = g.provider, model_id = g.model, updated_at = NOW()
FROM best_global g
WHERE NOT EXISTS (
    SELECT 1 FROM ai_price_catalog c
    WHERE c.provider = pm.provider AND c.model = pm.model_id AND c.is_enabled = true
);

-- B3) loop_fallback_default DEVE essere non-thinking robusto (era gemini-2.5-pro
--     thinking -> hollow). Lo portiamo al miglior enabled non-thinking globale.
WITH best_global AS (
    SELECT provider, model FROM ai_price_catalog
    WHERE is_enabled = true AND supports_tool_use = true AND agentic_thinking_policy = 'none'
    ORDER BY is_featured DESC, input_cost_per_million_tokens ASC NULLS LAST
    LIMIT 1
)
UPDATE nexus_purpose_model pm
SET provider = g.provider, model_id = g.model, updated_at = NOW()
FROM best_global g
WHERE pm.purpose = 'loop_fallback_default'
  AND EXISTS (
      SELECT 1 FROM ai_price_catalog c
      WHERE c.provider = pm.provider AND c.model = pm.model_id
        AND c.agentic_thinking_policy <> 'none'
  );
