-- Migrazione 0207: nodo understanding dedicato (Cluster 2).
--
-- Nuovo nodo del grafo LangGraph PRIMA del planner, gated da complessita':
-- comprende il problema prima di pianificare via (a) grounding semantico sul
-- codebase/KB/chat (riusa nexus_search_semantic), (b) fan-out di sub-agent
-- explore in parallelo (riusa il tool MCP dispatch_subagent). Produce un
-- context_brief vettoriale-informato che alimenta il planner.
--
-- Tutto default-OFF. Modello di sintesi economico via purpose 'understanding'
-- tier light (regola G). Flag OFF => il nodo e' pass-through, path identico.

INSERT INTO settings (key, value, category, description) VALUES
    ('orchestrator.understanding_enabled', 'false', 'orchestrator',
     'Se true, prima del planner un nodo understanding fa grounding semantico (+ fan-out explore opzionale) per task complessi.'),
    ('orchestrator.understanding_fanout_enabled', 'false', 'orchestrator',
     'Se true (e subagents abilitati), l''understanding spawna sub-agent explore in parallelo via dispatch_subagent.'),
    ('orchestrator.understanding_synthesize_enabled', 'false', 'orchestrator',
     'Se true, il context_brief viene sintetizzato da un LLM economico; altrimenti concatenazione strutturata dei risultati RAG.'),
    ('orchestrator.understanding_topk', '8', 'orchestrator',
     'Numero di hit della ricerca semantica per il grounding.'),
    ('orchestrator.understanding_min_token_budget', '3000', 'orchestrator',
     'Gate hard: sotto questo budget il nodo understanding non si attiva (task piccoli).'),
    ('orchestrator.understanding_max_explore', '3', 'orchestrator',
     'Massimo numero di sub-agent explore spawnati in parallelo dall''understanding.')
ON CONFLICT (key) DO NOTHING;

-- Purpose per la sintesi del context_brief: tier light, reasoning, no tool use.
INSERT INTO nexus_purpose_model (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes) VALUES
    ('understanding', 'openai', 'gpt-4o-mini', 'light', 'reasoning', false,
     'Cluster 2: sintesi del context_brief del nodo understanding (mig 0207)')
ON CONFLICT (purpose) DO UPDATE SET
    tier = EXCLUDED.tier,
    required_capability = EXCLUDED.required_capability,
    requires_tool_use = EXCLUDED.requires_tool_use,
    notes = EXCLUDED.notes,
    updated_at = NOW();
