-- 0186_chat_message_attachments.sql
--
-- Persistenza degli allegati alla chat AI: salva metadata in DB, file sul
-- filesystem (in `.nexus/attachments/<msg_id>/`). Opt-in per vettorializzazione
-- nella Knowledge Base del progetto via campo `kb_note_id`.
--
-- File path (filesystem) e mime_type permettono al frontend di mostrare chip
-- cliccabili / thumbnail per ogni allegato del messaggio. `indexed_at` traccia
-- quando il contenuto e' stato inserito nella KB (NULL = non indicizzato).
--
-- Nota: questa migrazione e' stata applicata sul DB dal task spawnato per il
-- design del task #69 (persistenza allegati chat). Il file SQL e' stato
-- ricreato nel main project per riallineare sqlx::migrate checksum dopo che
-- una pulizia automatica ha rimosso i file non-committed dal filesystem.

CREATE TABLE IF NOT EXISTS chat_message_attachments (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    message_id uuid NOT NULL REFERENCES chat_messages(id) ON DELETE CASCADE,
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    file_name text NOT NULL,
    file_path text NOT NULL,
    mime_type text NOT NULL,
    size_bytes bigint NOT NULL CHECK (size_bytes >= 0),
    kind text NOT NULL CHECK (kind IN ('text', 'image', 'binary')),
    kb_note_id uuid REFERENCES project_knowledge_notes(id) ON DELETE SET NULL,
    indexed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_chat_message_attachments_message
    ON chat_message_attachments(message_id);
CREATE INDEX IF NOT EXISTS idx_chat_message_attachments_project
    ON chat_message_attachments(project_id);
CREATE INDEX IF NOT EXISTS idx_chat_message_attachments_kb_note
    ON chat_message_attachments(kb_note_id) WHERE kb_note_id IS NOT NULL;
