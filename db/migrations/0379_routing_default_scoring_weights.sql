-- 0379_routing_default_scoring_weights.sql
--
-- FASE 1 del consolidamento del selettore modello unico (vedi ADR 0030).
--
-- Introduce la riga SENTINELLA dei pesi di scoring di default in
-- nexus_intent_routing_requirements. Prima i pesi 0.35/0.25/0.20/0.20 erano
-- HARDCODED nel codice Rust (routing_matrix_auto_promoter.rs::select_models_for_requirement,
-- path slot-based) in violazione della regola G. Ora il punto unico
-- orchestrator::default_scoring_weights() li legge da questa riga (cache 60s).
--
-- intent='*' / behavior_mode='*' NON e' un intent reale: load_requirements lo
-- ESCLUDE dal materializzare la routing matrix (WHERE intent <> '*'); serve SOLO
-- come fonte dei pesi di default per i call site che non hanno un requirement
-- per-intent (routing slot-based e, in FASE 2, le viste runtime).
--
-- I valori replicano ESATTAMENTE i default storici hardcoded: nessun cambio di
-- comportamento osservabile (regressione zero). L'admin puo' ricalibrarli senza
-- redeploy (regola G).

INSERT INTO nexus_intent_routing_requirements
    (intent, behavior_mode, required_capabilities, requires_tool_use,
     preferred_tier, weight_tier, weight_cost, weight_context,
     weight_capabilities, cost_direction)
VALUES
    ('*', '*', '{}', false, 'medium', 0.35, 0.25, 0.20, 0.20, 'asc')
ON CONFLICT (intent, behavior_mode) DO UPDATE SET
    weight_tier         = EXCLUDED.weight_tier,
    weight_cost         = EXCLUDED.weight_cost,
    weight_context      = EXCLUDED.weight_context,
    weight_capabilities = EXCLUDED.weight_capabilities,
    cost_direction      = EXCLUDED.cost_direction;
