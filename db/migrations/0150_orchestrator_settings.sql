-- PR-1 Plan/Act/Verify: setting orchestrator + nexus_purpose_model entry.
--
-- Tutti i setting sono OFF di default per non rompere il comportamento
-- attuale dei 60 profili agente. L'admin attiva via UI o via SQL.
-- Cache TTL 60s lato brain (orchestrator_config.py).

-- Setting feature flag + tuning
INSERT INTO settings (key, value, category, description, updated_at) VALUES
    ('orchestrator.plan_phase_enabled',         'false',                                             'orchestrator', 'Feature flag globale per il planner_node (PR-1). Off -> grafo si comporta come oggi.', NOW()),
    ('orchestrator.plan_behavior_modes',        'automatico,continuo',                               'orchestrator', 'CSV dei behavior_mode che attivano il flusso plan/act/verify.', NOW()),
    ('orchestrator.plan_intents',               'code,implement,fix,refactor,scaffold_app,architecture', 'orchestrator', 'CSV degli intent eleggibili per il planner.', NOW()),
    ('orchestrator.plan_min_token_budget',      '2000',                                              'orchestrator', 'Sotto questa soglia di token_budget il planner viene saltato (chat brevi).', NOW()),
    ('orchestrator.planner_prompt_key',         'agent.planner.base',                                'orchestrator', 'Indirezione per varianti A/B del prompt del planner.', NOW()),
    ('orchestrator.todo_reminder_every_n_steps','5',                                                 'orchestrator', 'Iniezione system reminder TODO ogni N tool use.', NOW()),
    ('orchestrator.todo_reminder_min_todos',    '3',                                                 'orchestrator', 'Sotto questa soglia di todos pending nessun reminder iniettato (anti-spam chat brevi).', NOW()),
    ('orchestrator.verifier_enabled',           'false',                                             'orchestrator', 'Feature flag globale per il verifier_node (PR-2). Indipendente dal planner.', NOW()),
    ('orchestrator.max_verify_cycles',          '3',                                                 'orchestrator', 'Cap re-iterazioni executor<->verifier per singolo todo (PR-2).', NOW()),
    ('orchestrator.max_plan_revisions',         '2',                                                 'orchestrator', 'Cap replan strutturali ammessi dopo verifier exhaustion (PR-2).', NOW()),
    ('orchestrator.verifier_timeout_s',         '30.0',                                              'orchestrator', 'Timeout singolo criterion check (PR-2).', NOW())
ON CONFLICT (key) DO UPDATE SET
    value = EXCLUDED.value,
    category = EXCLUDED.category,
    description = EXCLUDED.description,
    updated_at = NOW();

-- nexus_purpose_model: aggiungiamo le righe per planner e verifier purpose.
-- Default seed Anthropic (modelli reali presenti in ai_price_catalog).
-- L'admin puo' cambiarli da /admin/billing o via UPDATE diretto.
INSERT INTO nexus_purpose_model (purpose, provider, model_id, notes, updated_at) VALUES
    ('planner',  'anthropic', 'claude-sonnet-4-6',         'Modello usato dal planner_node: capable, low-latency, JSON output reliable.', NOW()),
    ('verifier', 'anthropic', 'claude-haiku-4-5-20251001', 'Modello usato per la sintesi post-verifier (DoD synthesis). Il verifier vero e proprio e deterministico (no LLM).', NOW())
ON CONFLICT (purpose) DO UPDATE SET
    provider = EXCLUDED.provider,
    model_id = EXCLUDED.model_id,
    notes = EXCLUDED.notes,
    updated_at = NOW();
