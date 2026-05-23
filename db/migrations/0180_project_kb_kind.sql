-- Aggiunge colonna `kind` a project_knowledge_notes per supportare note
-- categoriche oltre a quelle auto-create da chat user.
--
-- Kind possibili:
--   'chat'       — nota auto-create da messaggio user (default storico)
--   'technical'  — nota architettura/API/schema/file structure del progetto
--   'functional' — feature, requirement, decision, domain dal cluster intent
--   'test'       — nota descrittiva di test file (Playwright, Rust, pytest)
--   'manual'     — nota creata a mano via UI/tool

ALTER TABLE project_knowledge_notes
    ADD COLUMN IF NOT EXISTS kind TEXT NOT NULL DEFAULT 'chat';

CREATE INDEX IF NOT EXISTS idx_pkn_kind
    ON project_knowledge_notes(project_id, kind);
