-- 0348_project_documents_unique_path.sql
--
-- Bug osservato: per lo stesso file fisico sono stati creati due record in
-- project_documents con file_path differenti:
--   A) "docs/functional-analysis-v1.0.0.docx"                  (RELATIVO,
--      inserito da nexus_doc_generate)
--   B) "/home/administrator/projects/<slug>/docs/...docx"      (ASSOLUTO,
--      inserito dall'auto-discovery di GET /documents)
-- La guardia WHERE NOT EXISTS dell'auto-discovery confrontava col path assoluto
-- e non vedeva il record relativo gia' inserito -> duplicato.
--
-- Fix a due livelli (regola H, niente toppa):
--   1) Codice: auto-discovery normalizza il path al RELATIVO (vedi commit
--      successivo su documents.rs::list_documents).
--   2) Schema: UNIQUE constraint come rete di sicurezza, cosi' anche un futuro
--      call site con path inconsistente viene RIFIUTATO dal DB invece di
--      creare silenziosamente duplicati.
--
-- Cleanup pre-constraint: rimuove tutti i duplicati gia' presenti per
-- (project_id, file basename) tenendo il record piu' vecchio. Il basename viene
-- estratto dal file_path con un'espressione idempotente.

-- 1) Dedup pre-constraint: per ciascun (project_id, basename) tieni il record
--    con created_at piu' vecchio, elimina gli altri.
WITH ranked AS (
    SELECT
        id,
        ROW_NUMBER() OVER (
            PARTITION BY project_id,
                         regexp_replace(file_path, '^.*/', '')
            ORDER BY created_at ASC, id ASC
        ) AS rn
    FROM project_documents
)
DELETE FROM project_documents pd
USING ranked r
WHERE pd.id = r.id AND r.rn > 1;

-- 2) UNIQUE constraint su (project_id, file_path). Nominale, idempotente.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'project_documents_project_id_file_path_key'
    ) THEN
        ALTER TABLE project_documents
        ADD CONSTRAINT project_documents_project_id_file_path_key
        UNIQUE (project_id, file_path);
    END IF;
END$$;
