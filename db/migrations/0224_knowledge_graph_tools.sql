-- Migrazione 0224: colonne e vincoli per i nuovi tool MCP di grafo (Componente 0).
--
-- Abilita i tool knowledge_get_links / knowledge_get_subgraph /
-- knowledge_create_link / knowledge_set_relevance (agent_tools/knowledge.rs)
-- e prepara il terreno per l'Intake Gate (Componente 1, off_topic) e per
-- l'import di grafi esterni (Componente 2, created_by='external').
--
-- 1. off_topic + relevance_score su project_knowledge_notes: una nota marcata
--    off_topic resta in KB ma e' esclusa da grafo/RAG/DAG (filtro a valle).
-- 2. created_by del link esteso a 'agent' (link creati dall'agente/gate) e
--    'external' (link importati da grafi esterni). Il CHECK originario
--    (mig 0175) ammetteva solo 'auto','user'.

ALTER TABLE project_knowledge_notes
  ADD COLUMN IF NOT EXISTS off_topic BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE project_knowledge_notes
  ADD COLUMN IF NOT EXISTS relevance_score REAL;

CREATE INDEX IF NOT EXISTS idx_pkn_off_topic
  ON project_knowledge_notes(project_id, off_topic);

ALTER TABLE project_knowledge_links
  DROP CONSTRAINT IF EXISTS project_knowledge_links_created_by_check;

ALTER TABLE project_knowledge_links
  ADD CONSTRAINT project_knowledge_links_created_by_check
  CHECK (created_by IN ('auto','user','agent','external'));
