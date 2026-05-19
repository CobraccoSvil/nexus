-- Meta-step pubblicati in chat (plan/routing/clarify/fallback/reflection).
--
-- Schema persistenza opzionale per audit/storia: il canale primario di
-- pubblicazione e' SSE realtime (eventi `meta_step` ritrasmessi da
-- brain Python -> mcp-core -> frontend). Questa tabella consente di
-- ricostruire la timeline di un run a posteriori (es. trace panel,
-- dashboard analytics, retro debug).
--
-- Insert avviene best-effort da brain/agents/meta_steps.py (psycopg2).
-- Failure dell'insert NON blocca il run: gli eventi SSE arrivano comunque.

CREATE TABLE IF NOT EXISTS nexus_agent_meta_steps (
    id BIGSERIAL PRIMARY KEY,
    run_id UUID NOT NULL,
    kind TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    correlation_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_nexus_agent_meta_steps_run
    ON nexus_agent_meta_steps (run_id, created_at);
CREATE INDEX IF NOT EXISTS idx_nexus_agent_meta_steps_kind
    ON nexus_agent_meta_steps (kind);

COMMENT ON TABLE nexus_agent_meta_steps IS
    'Step semantici pubblicati in chat (plan, routing, clarify, fallback, reflection). '
    'Canale primario SSE; questa tabella e'' la copia persistente per audit/timeline.';

-- Feature flag per kind. Globale `global_enabled` permette kill-switch totale.
-- `reflection_enabled` OFF di default perche' aggiunge una chiamata LLM extra
-- a fine turno (costo) — gli altri sono economici (no LLM aggiuntivo).
INSERT INTO settings (key, value, category, description, updated_at) VALUES
    ('orchestrator.meta_steps.global_enabled',     'true',  'orchestrator', 'Kill-switch globale per i meta_step in chat (plan/routing/clarify/fallback/reflection).', NOW()),
    ('orchestrator.meta_steps.plan_enabled',       'true',  'orchestrator', 'Pubblica il piano del planner_node come meta_step kind=plan.', NOW()),
    ('orchestrator.meta_steps.routing_enabled',    'true',  'orchestrator', 'Pubblica la decisione di routing/profile come meta_step kind=routing.', NOW()),
    ('orchestrator.meta_steps.clarify_enabled',    'true',  'orchestrator', 'Pubblica le richieste di chiarimento (Fase 2 clarify_or_expand) come meta_step kind=clarify.', NOW()),
    ('orchestrator.meta_steps.fallback_enabled',   'true',  'orchestrator', 'Pubblica i fallback automatici di provider/modello come meta_step kind=fallback.', NOW()),
    ('orchestrator.meta_steps.reflection_enabled', 'false', 'orchestrator', 'Pubblica la riflessione post-hoc come meta_step kind=reflection. Off di default (costo LLM extra).', NOW())
ON CONFLICT (key) DO UPDATE SET
    value = EXCLUDED.value,
    category = EXCLUDED.category,
    description = EXCLUDED.description,
    updated_at = NOW();
