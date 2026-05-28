-- Mig 0186: persistenza degli allegati alla chat.
--
-- Prima di questa migrazione gli allegati (immagini incollate, file di testo
-- trascinati nel chat-panel) vivevano solo nel turno LLM e poi sparivano.
-- Ora vengono:
--   1. Salvati su filesystem in <project_root>/.nexus/attachments/<msg_id>/<file_safe>
--   2. Tracciati in questa tabella con metadata (mime, dimensione, kind)
--   3. Opzionalmente indicizzati nella Knowledge Base del progetto
--      (collegamento a project_knowledge_notes via kb_note_id)
--
-- La policy di cancellazione e' a cascata dal messaggio o dal progetto: se
-- l'utente cancella il messaggio o il progetto, gli allegati relativi
-- spariscono dal DB. Il file su disco viene gestito dal cleanup di progetto.
--
-- Niente DROP, niente rename: migrazione puramente additiva e idempotente.

BEGIN;

CREATE TABLE IF NOT EXISTS chat_message_attachments (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    message_id    UUID NOT NULL REFERENCES chat_messages(id) ON DELETE CASCADE,
    project_id    UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    file_name     TEXT NOT NULL,
    file_path     TEXT NOT NULL,
    mime_type     TEXT NOT NULL,
    size_bytes    BIGINT NOT NULL,
    kind          TEXT NOT NULL,
    kb_note_id    UUID REFERENCES project_knowledge_notes(id) ON DELETE SET NULL,
    indexed_at    TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT chat_message_attachments_kind_chk
        CHECK (kind IN ('text', 'image', 'binary')),
    CONSTRAINT chat_message_attachments_size_chk
        CHECK (size_bytes >= 0)
);

CREATE INDEX IF NOT EXISTS idx_chat_message_attachments_message
    ON chat_message_attachments(message_id);

CREATE INDEX IF NOT EXISTS idx_chat_message_attachments_project
    ON chat_message_attachments(project_id);

CREATE INDEX IF NOT EXISTS idx_chat_message_attachments_kb_note
    ON chat_message_attachments(kb_note_id)
    WHERE kb_note_id IS NOT NULL;

COMMIT;
