-- 0321_reconcile_default_models.sql  (ADR 0025)
--
-- Dopo il prune (mig 0320) i "modelli di default per provider" potevano puntare a
-- modelli ora DISABILITATI/legacy, in DUE config stratificate:
--   - nexus_provider_default_model (es. mistral -> mistral-large-2411 disabilitato)
--   - settings.provider_model_<provider> (es. deepseek -> deepseek-chat, legacy)
-- Risultato: forzando un provider dal dropdown si risolveva un modello disabilitato.
--
-- Riconcilia ENTRAMBE al miglior modello ENABLED agentic-eligibile del provider
-- (featured prima, poi costo crescente; esclude policy 'exclude'). Deterministico
-- e idempotente: tocca solo le righe il cui modello corrente NON e' enabled.

-- Miglior modello enabled per provider (agentic-eligibile).
WITH best AS (
    SELECT provider, model FROM (
        SELECT provider, model,
               row_number() OVER (
                   PARTITION BY provider
                   ORDER BY is_featured DESC, input_cost_per_million_tokens ASC NULLS LAST
               ) AS rn
        FROM ai_price_catalog
        WHERE is_enabled = true
          AND supports_tool_use = true
          AND agentic_thinking_policy <> 'exclude'
    ) t WHERE rn = 1
)
UPDATE nexus_provider_default_model d
SET model_id = b.model
FROM best b
WHERE b.provider = d.provider
  AND NOT EXISTS (
      SELECT 1 FROM ai_price_catalog c
      WHERE c.provider = d.provider AND c.model = d.model_id AND c.is_enabled = true
  );

-- settings.provider_model_<provider>: stessa riconciliazione.
WITH best AS (
    SELECT provider, model FROM (
        SELECT provider, model,
               row_number() OVER (
                   PARTITION BY provider
                   ORDER BY is_featured DESC, input_cost_per_million_tokens ASC NULLS LAST
               ) AS rn
        FROM ai_price_catalog
        WHERE is_enabled = true
          AND supports_tool_use = true
          AND agentic_thinking_policy <> 'exclude'
    ) t WHERE rn = 1
)
UPDATE settings s
SET value = b.model, updated_at = NOW()
FROM best b
WHERE s.key = 'provider_model_' || b.provider
  AND NOT EXISTS (
      SELECT 1 FROM ai_price_catalog c
      WHERE c.provider = b.provider AND c.model = s.value AND c.is_enabled = true
  );
