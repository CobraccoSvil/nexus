-- 0248_kb_lifecycle_deprecate_archive.sql
-- Fase C (M14) — Completa il lifecycle delle note KB: deprecazione su
-- correzione (M14.2) e archiviazione delle note active inattive (M14.5).
--
-- Stati nota: draft -> active -> {deprecated | archived}.
--   - deprecated_at: la nota e' stata superata da un summary successivo sugli
--     stessi file (deprecate_notes_on_correction). superseded_by punta alla
--     nota che la sostituisce.
--   - archived_at: la nota e' stata archiviata dal worker di cleanup (draft
--     troppo vecchie: gia' attivo; oppure active inattive da troppo tempo:
--     nuovo, gated da knowledge.cleanup_inactive_enabled).
--
-- Il worker di archiviazione gia' esiste (knowledge_workers.rs::cleanup_tick) e
-- usa il namespace knowledge.cleanup_*: si estendono quelle chiavi invece di
-- introdurne di nuove duplicate (regola H, niente duplicazione). Niente valori
-- hardcoded nel codice (regola G). Idempotente.

ALTER TABLE project_knowledge_notes
    ADD COLUMN IF NOT EXISTS deprecated_at timestamptz;
ALTER TABLE project_knowledge_notes
    ADD COLUMN IF NOT EXISTS superseded_by uuid;
ALTER TABLE project_knowledge_notes
    ADD COLUMN IF NOT EXISTS archived_at   timestamptz;

-- Indice per la passata di archiviazione: scandisce note non gia' archiviate
-- filtrando per stato + ultima modifica.
CREATE INDEX IF NOT EXISTS idx_notes_lifecycle_sweep
    ON project_knowledge_notes USING btree (project_id, status, updated_at)
    WHERE (archived_at IS NULL);

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('knowledge.cleanup_inactive_days', '90', 'knowledge',
     'Eta'' (giorni) dall''ultimo updated_at oltre cui una nota active viene archiviata dal worker di cleanup (M14.5).', 'f'),
    ('knowledge.cleanup_inactive_enabled', 'false', 'knowledge',
     'Gate M14.5 per l''archiviazione delle note active inattive. OFF di default: non archiviare note attive a sorpresa. L''archiviazione delle draft vecchie resta sempre attiva.', 'f')
ON CONFLICT (key) DO NOTHING;
