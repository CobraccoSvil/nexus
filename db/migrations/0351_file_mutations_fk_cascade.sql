-- 0351_file_mutations_fk_cascade.sql
--
-- Fix: la tabella `file_mutations` (mig 0349) e' stata creata con
-- `project_id UUID NOT NULL` ma SENZA la FK verso projects(id). Conseguenza:
-- alla cancellazione di un progetto i record file_mutations restano orfani
-- nel DB (il CASCADE non scatta su una colonna non vincolata).
--
-- Diagnostica trovata nell'audit della procedura DELETE /api/projects/:id.
-- Tutte le altre tabelle correlate hanno FK CASCADE corretta; questa era
-- l'unica eccezione.
--
-- Cleanup pre-vincolo: rimuove eventuali record gia' orfani (project_id che
-- non esiste piu' in `projects`). Idempotente.

DELETE FROM file_mutations
 WHERE project_id NOT IN (SELECT id FROM projects);

-- Aggiungi il vincolo solo se manca (idempotente per riesecuzioni).
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'file_mutations_project_id_fkey'
    ) THEN
        ALTER TABLE file_mutations
        ADD CONSTRAINT file_mutations_project_id_fkey
        FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE;
    END IF;
END$$;
