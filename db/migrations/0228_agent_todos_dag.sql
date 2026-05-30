-- Migrazione 0228: schema DAG per il coordinamento delle azioni (Componente 3a).
--
-- Estende nexus_agent_todos con le dipendenze tra todo, cosi' che l'esecuzione
-- possa rispettare un ordine topologico derivato dal grafo KB invece del solo
-- seq lineare. Il planner ragiona su chiavi logiche (node_key / dep_keys), che
-- mcp-core risolve in depends_on (UUID[]) lato Rust (il planner non conosce gli
-- UUID generati). dag_layer e' il livello topologico calcolato (per la
-- parallelizzazione opt-in del Comp.3b).
--
-- Default-OFF: con dag_topological_enabled=false (o depends_on vuoto su tutti i
-- todo) il verifier sceglie il prossimo todo per seq, comportamento identico a
-- oggi.

ALTER TABLE nexus_agent_todos
  ADD COLUMN IF NOT EXISTS depends_on UUID[] NOT NULL DEFAULT '{}';

ALTER TABLE nexus_agent_todos
  ADD COLUMN IF NOT EXISTS dep_keys TEXT[];

ALTER TABLE nexus_agent_todos
  ADD COLUMN IF NOT EXISTS node_key TEXT;

ALTER TABLE nexus_agent_todos
  ADD COLUMN IF NOT EXISTS dag_layer INTEGER;

CREATE INDEX IF NOT EXISTS idx_todos_depends_on
  ON nexus_agent_todos USING GIN (depends_on);

INSERT INTO settings (key, value, category, description) VALUES
    ('orchestrator.dag_topological_enabled', 'false', 'orchestrator',
     'Comp.3a: se true, il verifier sceglie il prossimo todo rispettando depends_on (ordine topologico) invece del solo seq lineare.')
ON CONFLICT (key) DO NOTHING;
