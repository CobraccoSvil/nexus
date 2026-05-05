-- Migrazione 0115: vincolo UNIQUE su (project_id, file_path) per project_documents
--
-- Scopo: prevenire duplicati creati dall'auto-discovery dei .md in docs/
-- (vedi crates/mcp-core/src/documents.rs::list_documents). Senza questo
-- vincolo, chiamate concorrenti dell'endpoint list_documents possono
-- inserire piu' record per lo stesso (project_id, file_path) — caso reale
-- con React StrictMode che invoca l'effect due volte.
--
-- Pulizia: prima di creare l'index dobbiamo rimuovere i duplicati esistenti,
-- mantenendo il record piu' vecchio (created_at minore) per ogni
-- (project_id, file_path) — l'id MIN garantisce stabilita' anche se
-- created_at coincide al microsecondo.

DELETE FROM project_documents
WHERE id IN (
    SELECT id FROM (
        SELECT id,
               ROW_NUMBER() OVER (
                   PARTITION BY project_id, file_path
                   ORDER BY created_at, id
               ) AS rn
        FROM project_documents
    ) t
    WHERE t.rn > 1
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_project_documents_path
    ON project_documents (project_id, file_path);
