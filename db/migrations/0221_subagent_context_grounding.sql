-- Migrazione 0221: contesto continuo via RAG ai sub-agent (Componente B).
--
-- I sub-agent, invece di partire "ciechi", ricevono nel system_text (a) un
-- grounding sulla memoria vettoriale LOCALE del progetto (via il servizio
-- esistente /api/internal/knowledge/search) e (b) il rationale del piano del
-- parent (da nexus_agent_plans, mig 0206). NIENTE dump della conversazione del
-- parent: solo snippet locali + rationale strutturato (riservatezza dati).
-- Tutto default-OFF. snippet_max e topk limitano costo e superficie dati.

INSERT INTO settings (key, value, category, description) VALUES
    ('orchestrator.subagent_rag_grounding_enabled', 'false', 'orchestrator',
     'Se true, i sub-agent ricevono un grounding sulla memoria vettoriale del progetto (ricerca semantica locale) nel system_text.'),
    ('orchestrator.subagent_rag_grounding_topk', '5', 'orchestrator',
     'Numero di note recuperate per il grounding del sub-agent.'),
    ('orchestrator.subagent_rag_grounding_min_score', '0.55', 'orchestrator',
     'Soglia minima di similarita'' per il grounding del sub-agent.'),
    ('orchestrator.subagent_rag_grounding_snippet_max', '800', 'orchestrator',
     'Cap caratteri per snippet del grounding (controllo costi + superficie dati verso il provider).'),
    ('orchestrator.subagent_inherit_plan_rationale', 'false', 'orchestrator',
     'Se true, il sub-agent riceve il rationale del piano del parent (nexus_agent_plans), solo strutturato.')
ON CONFLICT (key) DO NOTHING;
