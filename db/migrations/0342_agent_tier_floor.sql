-- 0342_agent_tier_floor.sql
--
-- Floor selettivo del tier per i task AGENTICI. Il modello dell'executor e'
-- risolto per (intent x behavior_mode); sotto modalita' "veloce"/"economica" un
-- task agentico "pesante" (loop tool-use multi-step) puo' cadere su un modello
-- lite, dove il tool forcing fallisce. Questi settings governano il floor
-- applicato in brain/agents/nodes/helpers.py::apply_agentic_tier_floor: se
-- agentic_score >= min OPPURE iteration_budget >= min, e la modalita' e'
-- veloce/economica, si eleva al `mode` (bilanciata) per ottenere un modello
-- tool-robust. La distinzione e' semantica (agentic_score / budget), non keyword
-- (a differenza del vecchio is_risky_task, rimosso). DB-driven (regola G).
-- Idempotente: ON CONFLICT DO NOTHING (non sovrascrive override admin).

INSERT INTO settings (key, value, category, description) VALUES
(
    'agent.tier_floor.enabled', 'true', 'agent',
    'Se true, i task agentici pesanti in modalita'' veloce/economica vengono elevati a un tier tool-robust (vedi apply_agentic_tier_floor).'
),
(
    'agent.tier_floor.agentic_score_min', '0.6', 'agent',
    'Soglia di agentic_score (>=) oltre cui un task e'' considerato agentico ai fini del floor del tier.'
),
(
    'agent.tier_floor.iteration_budget_min', '160', 'agent',
    'Soglia di iteration_budget (>=) oltre cui un task e'' considerato pesante ai fini del floor del tier.'
),
(
    'agent.tier_floor.mode', 'bilanciata', 'agent',
    'behavior_mode minimo applicato ai task agentici pesanti quando la modalita'' richiesta e'' veloce/economica.'
)
ON CONFLICT (key) DO NOTHING;
