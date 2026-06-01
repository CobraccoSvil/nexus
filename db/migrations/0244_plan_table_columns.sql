-- 0244_plan_table_columns.sql
-- M14 + M15 — Colonne aggiunte a tabelle esistenti dal piano.
--
-- project_knowledge_notes.context_stale_at (M14.3): timestamp di quando i
--   file_paths della nota sono divergati per un run successivo non collegato.
--   Settato da knowledge/mod.rs::mark_context_stale_notes, letto dall'intake.
-- nexus_agent_todos.carry_over / origin_run_id / edited_by (M15.3/M15.4):
--   persistenza cross-run del backlog + tracciamento autore modifica todo.
-- Ricostruzione fedele allo schema applicato in produzione. Idempotente.

ALTER TABLE project_knowledge_notes
    ADD COLUMN IF NOT EXISTS context_stale_at timestamptz;

ALTER TABLE nexus_agent_todos
    ADD COLUMN IF NOT EXISTS carry_over    boolean NOT NULL DEFAULT false;
ALTER TABLE nexus_agent_todos
    ADD COLUMN IF NOT EXISTS origin_run_id uuid;
ALTER TABLE nexus_agent_todos
    ADD COLUMN IF NOT EXISTS edited_by     text;

CREATE INDEX IF NOT EXISTS idx_todos_carryover
    ON nexus_agent_todos USING btree (project_id, carry_over)
    WHERE (carry_over = true);
