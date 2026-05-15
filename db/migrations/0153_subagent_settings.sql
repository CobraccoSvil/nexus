-- PR-3 sub-agents: setting categoria orchestrator.
-- Tutti default OFF / safe per non rompere comportamento esistente.

INSERT INTO settings (key, value, category, description, updated_at) VALUES
    ('orchestrator.subagents_enabled',              'false',                                 'orchestrator', 'Feature flag globale sub-agents pattern. Off -> dispatch_subagent ritorna errore al main.', NOW()),
    ('orchestrator.subagent_kinds_whitelist',       'plan,explore,implement,verify,review',  'orchestrator', 'CSV dei kind ammessi per dispatch_subagent (filtra anche custom kinds).', NOW()),
    ('orchestrator.max_parallel_subagents',         '3',                                     'orchestrator', 'Concorrenza max sub-agent in-flight per singolo parent run.', NOW()),
    ('orchestrator.subagent_max_depth',             '2',                                     'orchestrator', 'Profondita max di annidamento sub-agent (sub-of-sub).', NOW()),
    ('orchestrator.subagent_default_timeout_s',     '300',                                   'orchestrator', 'Timeout default per kind se non specificato in nexus_subagent_definitions.', NOW()),
    ('orchestrator.subagent_cost_cap_per_run_usd',  '5.00',                                  'orchestrator', 'Hard cap di spesa cumulativa sub-agents per singolo parent run.', NOW())
ON CONFLICT (key) DO UPDATE SET
    value = EXCLUDED.value,
    description = EXCLUDED.description,
    updated_at = NOW();
