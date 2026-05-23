-- Knowledge Base context injection nel system prompt agente.
-- Quando un messaggio user arriva, mcp-core embed-a il testo, cerca top-K note
-- simili in Qdrant `knowledge_notes` filtrate per project_id, e le prepende
-- al system_prompt come "Contesto dal Knowledge Base del progetto".

INSERT INTO settings (key, value, category, description) VALUES
    ('knowledge.context_injection_enabled', 'true', 'knowledge',
     'Abilita iniezione automatica delle note KB rilevanti nel system prompt agente'),
    ('knowledge.context_injection_top_k', '5', 'knowledge',
     'Numero massimo di note KB da iniettare nel system prompt (clamp 1-20)'),
    ('knowledge.context_injection_min_score', '0.5', 'knowledge',
     'Soglia minima di similarita cosine (0-1) per includere una nota nel contesto')
ON CONFLICT (key) DO NOTHING;
