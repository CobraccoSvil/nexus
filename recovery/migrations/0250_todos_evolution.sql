-- 0250_todos_evolution.sql
--
-- M15.3 (todo editabili dall'utente) + M15.4 (persistenza cross-run).
--
-- Estende nexus_agent_todos con:
--   - edited_by: provenienza dell'ultima modifica ('user'|'agent'; NULL = agent,
--     default storico). Permette di distinguere i todo toccati dall'utente da
--     quelli generati/aggiornati dal planner/executor.
--   - carry_over: marca un todo da ereditare nei run successivi dello stesso
--     progetto. A fine run i todo 'pending'/'blocked' NON vengono cancellati
--     (regola H): vengono marcati carry_over=true cosi' il planner del run
--     successivo puo' ereditarli come backlog.
--   - origin_run_id: run in cui il todo e' stato originariamente creato. Resta
--     stabile anche quando il todo migra come backlog tra run.
--
-- Default-safe: con carry_over_enabled=false la marcatura a fine run e' inerte
-- e il comportamento resta identico a oggi.

ALTER TABLE nexus_agent_todos
  ADD COLUMN IF NOT EXISTS edited_by TEXT;

ALTER TABLE nexus_agent_todos
  ADD COLUMN IF NOT EXISTS carry_over BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE nexus_agent_todos
  ADD COLUMN IF NOT EXISTS origin_run_id UUID;

-- Indice per la query di backlog cross-run (status IN pending/blocked + carry_over
-- per project, escludendo il run corrente). project_id e' gia' indicizzato ma
-- l'indice composito copre direttamente il predicato del backlog.
CREATE INDEX IF NOT EXISTS idx_todos_carryover
  ON nexus_agent_todos(project_id, carry_over)
  WHERE carry_over = true;

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('agent.todos.user_editable', 'true', 'agent',
     'M15.3: abilita l''endpoint POST /api/agent/todos/{run_id}/edit per modificare i todo del piano dall''interfaccia utente (add/edit/reorder/remove).', FALSE),
    ('agent.todos.carry_over_enabled', 'true', 'agent',
     'M15.4: a fine run i todo pending/blocked vengono marcati carry_over=true (con origin_run_id) invece di restare orfani, cosi'' il planner del run successivo li eredita come backlog.', FALSE)
ON CONFLICT (key) DO NOTHING;
