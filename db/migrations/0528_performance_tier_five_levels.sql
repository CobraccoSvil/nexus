-- 0528_performance_tier_five_levels.sql
-- Scala di capacita' dei modelli da 3 a 5 livelli: light < medium < high < heavy <
-- frontier (fase 2 della feature tier a 5 livelli; la fase 1, solo-codice, e' gia'
-- retro-compatibile e in produzione).
--
-- Causa radice (regola H): con 3 tier (light/medium/heavy) tutti i modelli di
-- fascia alta collassavano in 'heavy' (gemini-2.5-pro, deepseek-v4-pro, gpt-5.5,
-- opus-4-8), quindi escalation e selezione non distinguevano il "meglio
-- disponibile". La riclassificazione a 5 livelli (infer_tier_from_name, applicata a
-- ogni catalog_sync alle righe capability_source='auto') distribuisce la fascia alta
-- su high/heavy/frontier; le righe 'manual' restano protette. La telemetria
-- (governance) affina a valle col dato reale.
--
-- (1) CHECK esteso ai 5 valori. DO-block robusto: droppa il constraint check su
--     performance_tier qualunque sia il suo nome, poi ricrea con i 5 livelli.
-- (2) Vista v_model_escalation_chain ricreata: i due CASE tier->ord (in
--     escalation_rank e performance_tier_ord) mappano ora i 5 tier
--     (light 0 / medium 1 / high 2 / heavy 3 / frontier 4). Il tier_ord domina
--     ancora escalation_rank (fattore 1_000_000 >> blended_cost*1000). Il
--     cost_bucket resta una metrica di COSTO (soglie 0.5/3.0), indipendente dal
--     performance_tier: il suo valore 'frontier' (costo alto) e' un concetto
--     distinto dal tier 'frontier' (capacita').

DO $$
DECLARE cname text;
BEGIN
    SELECT conname INTO cname
      FROM pg_constraint
     WHERE conrelid = 'ai_price_catalog'::regclass
       AND contype = 'c'
       AND pg_get_constraintdef(oid) ILIKE '%performance_tier%';
    IF cname IS NOT NULL THEN
        EXECUTE format('ALTER TABLE ai_price_catalog DROP CONSTRAINT %I', cname);
    END IF;
END $$;

ALTER TABLE ai_price_catalog
    ADD CONSTRAINT ai_price_catalog_performance_tier_check
    CHECK (performance_tier IN ('light', 'medium', 'high', 'heavy', 'frontier'));

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
            WHEN 'high' THEN 2
            WHEN 'heavy' THEN 3
            WHEN 'frontier' THEN 4
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
        WHEN 'high' THEN 2
        WHEN 'heavy' THEN 3
        WHEN 'frontier' THEN 4
        ELSE 1
     END) AS performance_tier_ord
FROM ai_price_catalog
WHERE is_enabled = TRUE;

COMMENT ON VIEW v_model_escalation_chain IS
    'Catena di escalation derivata dal catalog (mig 0471/0475, tier a 5 livelli mig '
    '0528, regola L). Proiezione pura di ai_price_catalog (solo is_enabled) con '
    'blended_cost, escalation_rank, cost_bucket e performance_tier_ord derivati. '
    'tier_ord: light 0 / medium 1 / high 2 / heavy 3 / frontier 4. Ordina per '
    'escalation_rank ASC per la catena ricca.';
