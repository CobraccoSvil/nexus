-- 0220_chat_attachment_content_hash.sql
--
-- Storage allegati chat content-addressed e deduplicato.
--
-- Prima di questa migrazione gli allegati venivano salvati in
-- `.nexus/attachments/<message_id>/<file>` (directory = UUID del messaggio):
--   - directory illeggibile per l'utente nell'Explorer
--   - lo stesso file caricato in messaggi diversi finiva duplicato su disco
--
-- Da ora i nuovi upload usano uno schema content-addressed e leggibile:
--   `.nexus/attachments/<safe_name>-<hash8>/<safe_name>`
-- dove `<hash8>` sono i primi 8 hex char di sha256(contenuto). Stesso contenuto
-- -> stessa cartella -> stesso file fisico (deduplica). File diversi con lo
-- stesso nome -> hash diversi -> cartelle diverse (nessuna collisione).
--
-- La colonna `content_hash` memorizza lo sha256 esadecimale completo (64 char)
-- del contenuto. Piu' record (di messaggi diversi) che puntano allo stesso
-- contenuto condividono `file_path` e `content_hash`: l'indice
-- (project_id, content_hash) abilita query di dedup e cleanup orfani.
--
-- I vecchi allegati gia' salvati sotto <message_id>/ restano dove sono: il loro
-- `file_path` nel DB continua a puntare al path legacy; `content_hash` resta
-- NULL per i record pre-esistenti. Lo schema nuovo vale solo per i nuovi upload.
--
-- Idempotente: ADD COLUMN IF NOT EXISTS + CREATE INDEX IF NOT EXISTS.

ALTER TABLE chat_message_attachments
    ADD COLUMN IF NOT EXISTS content_hash text;

CREATE INDEX IF NOT EXISTS idx_chat_message_attachments_project_hash
    ON chat_message_attachments(project_id, content_hash)
    WHERE content_hash IS NOT NULL;
