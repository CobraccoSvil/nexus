-- ADR 0017 v2 TODO 6 — flag idempotenza per il worker chat-note.
--
-- Il worker `wiki::chat_note_worker` scansiona periodicamente `chat_messages`
-- (role='user') e crea un wiki_doc (scope=project, kind='chat_note') per ogni
-- messaggio qualificato. Il flag `kb_ingested` evita di processare due volte
-- lo stesso messaggio.
--
-- NULL (default per le righe esistenti) == ancora da valutare. TRUE == gia'
-- ingestito (o esplicitamente scartato dal worker). Niente colonna FALSE
-- esplicita: il worker filtra "WHERE kb_ingested IS NULL".

BEGIN;

ALTER TABLE chat_messages
    ADD COLUMN IF NOT EXISTS kb_ingested BOOLEAN;

CREATE INDEX IF NOT EXISTS idx_chat_messages_kb_ingest_pending
    ON chat_messages (created_at)
    WHERE role = 'user' AND kb_ingested IS NULL;

COMMIT;
