-- Migrazione 0205: settings per worker-mode (PR-C) e attivazione adattiva (PR-D).
--
-- Tutti default conservativi/OFF: il sistema attuale resta identico finche'
-- l'admin non attiva esplicitamente i flag. Cache 60s lato brain (orchestrator_config).

INSERT INTO settings (key, value, category, description) VALUES
    -- PR-C: worker-mode (executor diventa orchestratore puro che delega).
    ('orchestrator.worker_mode_enabled', 'false', 'orchestrator',
     'Se true, nel run principale (subagent_depth=0) dopo il planner l''executor usa il prompt agent.orchestrator.base e tool ridotti: delega ai worker invece di implementare inline.'),
    ('orchestrator.worker_mode_tool_whitelist',
     'list_files,read_file,search_in_files,recall_context,search_codebase_semantic,nexus_todo_write,dispatch_subagent,nexus_subagent_poll,nexus_subagent_resume',
     'orchestrator',
     'Tool consentiti all''orchestratore in worker-mode (CSV). Solo lettura/coordinamento + delega; niente write/exec (li fanno i worker).'),

    -- PR-D: attivazione adattiva del planner forte da confidence/complessita'.
    ('orchestrator.adaptive_classifier_enabled', 'false', 'orchestrator',
     'Se true, router_node invoca il classifier agentico LLM e scrive complexity/agentic_score/is_ambiguous nello state.'),
    ('orchestrator.adaptive_gating_enabled', 'false', 'orchestrator',
     'Se true, is_eligible_adaptive usa i segnali del classifier per gate-are il planner forte (oltre ai gate hard budget/behavior).'),
    ('orchestrator.adaptive_agentic_score_min', '0.7', 'orchestrator',
     'Soglia di agentic_score sopra la quale attivare il planner forte.'),
    ('orchestrator.adaptive_low_confidence_max', '0.5', 'orchestrator',
     'Soglia di confidence sotto la quale (incertezza) attivare il planner forte.')
ON CONFLICT (key) DO NOTHING;
