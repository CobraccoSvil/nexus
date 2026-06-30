-- Migrazione 0104: purpose models per i 3 tier agente.
--
-- Rimuove le costanti hardcoded OPUS/SONNET/HAIKU in nexus_routing.rs
-- (righe 72-74) sostituendole con lookup da nexus_purpose_model.
-- Vedi regola G del CLAUDE.md: nomi modello mai hardcoded.
--
-- Dopo questa migrazione + deploy, un cambio di modello per tier si fa con:
--   UPDATE nexus_purpose_model SET provider='...', model_id='...'
--     WHERE purpose = 'agent_tier_opus';
-- e il refresh cache (<=60s) lo propaga a tutti gli agent run.

INSERT INTO nexus_purpose_model (purpose, provider, model_id, notes) VALUES
    ('agent_tier_opus',   'openai',   'gpt-4.1',      'tier alto: decisioni strutturali (Architect, SecurityArchitect, TechLead, ecc.)'),
    ('agent_tier_sonnet', 'openai',   'gpt-4.1-mini', 'tier medio: codifica/review ad alto volume (Coder, Reviewer, SRE, ecc.)'),
    ('agent_tier_haiku',  'deepseek', 'deepseek-chat', 'tier basso: task brevi/ripetitivi (Tester, TechWriter, monitoring, ecc.)')
ON CONFLICT (purpose) DO NOTHING;
