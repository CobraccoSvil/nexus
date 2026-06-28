-- 0475_escalation_routing_from_view.sql
-- LIVELLO A (budget-aware nella routing matrix) DERIVATO dalla stessa vista del
-- LIVELLO B: consolida la catena di escalation su un punto unico (regola L/G).
--
-- Causa radice (regola L): la mig 0471 ha gia' reso il LIVELLO B (loop
-- intra-provider) una proiezione del catalog (vista v_model_escalation_chain),
-- ma il LIVELLO A - le colonne escalation_* della nexus_routing_matrix usate da
-- lookup_with_budget per escalare quando il prompt supera una soglia di token -
-- era ancora popolato da COPPIE HARDCODED (const ESCALATION_PAIRS in
-- crates/mcp-core/src/models.rs: google/anthropic/openai con label di famiglia e
-- soglie inventate). Due implementazioni della stessa decisione "modello dello
-- stesso provider con tier superiore" -> violazione del punto unico.
--
-- Fix strutturale:
--   (a) la vista espone performance_tier_ord (light 0 / medium 1 / heavy 2) cosi'
--       il LIVELLO A puo' selezionare "il modello dello STESSO provider con tier
--       STRETTAMENTE superiore, piu' economico tra quelli, tool-capable" con la
--       stessa fonte dati del LIVELLO B (nessuna nuova classificazione di tier);
--   (b) la soglia di escalation diventa DB-driven (regola G): un solo setting
--       routing.escalation_budget_threshold_tokens, niente soglie sparse hardcoded.
--   La derivazione effettiva delle colonne escalation_* avviene in
--   auto_populate_escalations (models.rs), riscritta per leggere questa vista.
--
-- CREATE OR REPLACE su una vista esistente consente SOLO aggiunte di colonne in
-- coda: la definizione sotto e' copia ESATTA della mig 0471 (stesso ordine delle
-- colonne esistenti) con la sola nuova colonna performance_tier_ord aggiunta dopo
-- cost_bucket.

CREATE OR REPLACE VIEW v_model_escalation_chain AS
SELECT
    provider,
    model,
    performance_tier,
    speed_tier,
    is_enabled,
    consecutive_failures,
    consecutive_tool_failures,
    supports_tool_use,
    supports_vision,
    agentic_thinking_policy,
    capabilities,
    context_window,
    (input_cost_per_million_tokens * 0.75
        + output_cost_per_million_tokens * 0.25) AS blended_cost,
    (
        (CASE performance_tier
            WHEN 'light' THEN 0
            WHEN 'medium' THEN 1
            WHEN 'heavy' THEN 2
            ELSE 1
         END) * 1000000
        + round(
            (input_cost_per_million_tokens * 0.75
                + output_cost_per_million_tokens * 0.25) * 1000
          )
    )::bigint AS escalation_rank,
    CASE
        WHEN (input_cost_per_million_tokens * 0.75
                + output_cost_per_million_tokens * 0.25) < 0.5 THEN 'speed'
        WHEN (input_cost_per_million_tokens * 0.75
                + output_cost_per_million_tokens * 0.25) > 3.0 THEN 'frontier'
        ELSE 'value'
    END AS cost_bucket,
    (CASE performance_tier
        WHEN 'light' THEN 0
        WHEN 'medium' THEN 1
        WHEN 'heavy' THEN 2
        ELSE 1
     END) AS performance_tier_ord
FROM ai_price_catalog
WHERE is_enabled = TRUE;

COMMENT ON VIEW v_model_escalation_chain IS
    'Catena di escalation derivata dal catalog (mig 0471, estesa in 0475 con '
    'performance_tier_ord per il LIVELLO A budget-aware della routing matrix; '
    'regola L). Proiezione pura di ai_price_catalog (solo is_enabled) con '
    'blended_cost, escalation_rank, cost_bucket e performance_tier_ord derivati. '
    'Ordina per escalation_rank ASC per la catena ricca. Unica fonte di verita'' '
    'per LIVELLO A (escalation_* routing_matrix) e LIVELLO B (loop intra-provider).';

-- Soglia DB-driven (regola G): valore di bootstrap del setting, non un
-- magic-fallback. ON CONFLICT DO NOTHING preserva eventuali override admin.
INSERT INTO settings (key, value, category) VALUES
    ('routing.escalation_budget_threshold_tokens', '16000', 'routing')
ON CONFLICT (key) DO NOTHING;

-- Reset + ri-derivazione one-shot del LIVELLO A (regola H: fix definitivo, niente
-- dati sporchi residui). Le escalation_* materializzate dal vecchio sistema
-- (const ESCALATION_PAIRS) contenevano self-escalation (es. claude-opus -> se
-- stesso) e soglie legacy (8000/30000/50000/100000) disallineate dalla vista.
-- Azzeriamo le righe NON-manual e le ri-deriviamo dalla stessa logica di
-- auto_populate_escalations (Rust). I pin admin (manual_override=true) restano
-- intatti. auto_populate_escalations al boot e' idempotente (trova tutto popolato).
UPDATE nexus_routing_matrix
SET escalation_threshold_tokens = NULL,
    escalation_provider = NULL,
    escalation_model_id = NULL
WHERE (manual_override IS NULL OR manual_override = false);

UPDATE nexus_routing_matrix m
SET escalation_threshold_tokens = (
        SELECT COALESCE(value::int, 16000) FROM settings
        WHERE key = 'routing.escalation_budget_threshold_tokens'),
    escalation_provider = m.provider,
    escalation_model_id = (
        SELECT v.model FROM v_model_escalation_chain v
        WHERE v.provider = m.provider AND v.supports_tool_use = TRUE
          AND v.performance_tier_ord > (
              SELECT b.performance_tier_ord FROM v_model_escalation_chain b
              WHERE b.provider = m.provider AND b.model = m.model_id)
        ORDER BY v.escalation_rank ASC LIMIT 1),
    updated_at = NOW()
WHERE m.escalation_model_id IS NULL
  AND (m.manual_override IS NULL OR m.manual_override = false)
  AND EXISTS (
      SELECT 1 FROM v_model_escalation_chain v
      WHERE v.provider = m.provider AND v.supports_tool_use = TRUE
        AND v.performance_tier_ord > (
            SELECT b.performance_tier_ord FROM v_model_escalation_chain b
            WHERE b.provider = m.provider AND b.model = m.model_id));
