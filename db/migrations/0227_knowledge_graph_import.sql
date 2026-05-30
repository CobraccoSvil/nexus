-- Migrazione 0227: import di grafi esterni nella KB (Componente 2).
--
-- Permette di importare grafi prodotti da strumenti esterni (JSON node-link
-- canonico, Mermaid, DOT) e fonderli con la KB nativa. I nodi diventano note
-- (source_kind='external'), gli archi diventano link (created_by='external',
-- gia' ammesso da mig 0224). I nodi con relazioni di dipendenza alimentano il
-- DAG (Componente 3) come dipendenze HARD.

ALTER TABLE project_knowledge_notes
  ADD COLUMN IF NOT EXISTS source_kind TEXT NOT NULL DEFAULT 'native';

ALTER TABLE project_knowledge_notes
  ADD COLUMN IF NOT EXISTS external_source_id TEXT;

CREATE INDEX IF NOT EXISTS idx_pkn_source_kind
  ON project_knowledge_notes(project_id, source_kind);

INSERT INTO settings (key, value, category, description) VALUES
    ('knowledge.graph_import_enabled', 'true', 'knowledge',
     'Comp.2: abilita l''import di grafi esterni (JSON node-link, Mermaid, DOT) nella KB.'),
    ('knowledge.graph_import_max_nodes', '2000', 'knowledge',
     'Comp.2: numero massimo di nodi importabili in un singolo grafo.'),
    ('knowledge.graph_import_autolink', 'true', 'knowledge',
     'Comp.2: dopo l''import collega i nodi importati ai nativi (recompute_links).')
ON CONFLICT (key) DO NOTHING;
