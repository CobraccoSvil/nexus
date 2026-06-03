-- 0247_context_stale.sql
--
-- M14.3 del piano "Lifecycle della Knowledge Base".
--
-- Flag context-stale per le note KB. Una nota 'active' che documenta/copre certi
-- file diventa "stale" quando quei file vengono modificati da un run SUCCESSIVO
-- non collegato alla nota: va segnalata (NON cancellata), cosi' l'intake
-- successivo (M14.4) puo' avvisare l'utente che il contesto e' cambiato.
--
-- Meccanismo (vedi mark_context_stale_notes in crates/mcp-core/src/knowledge/
-- mod.rs): a fine run, per le note 'active' i cui file_paths intersecano i
-- files_touched dal run MA il cui source_run_id != run corrente (cioe' non sono
-- la nota prodotta da questo run) e context_stale_at IS NULL, si imposta
-- context_stale_at = NOW(). Il reset a NULL avviene quando la nota viene
-- ri-ingestata da un nuovo source_run che la copre (nuova nota).
--
-- Niente nuova tabella: aggiungiamo una sola colonna nullable a
-- project_knowledge_notes. L'indice GIN idx_pkn_file_paths_gin gia' esistente
-- supporta l'overlap su file_paths.

ALTER TABLE project_knowledge_notes
    ADD COLUMN IF NOT EXISTS context_stale_at TIMESTAMPTZ NULL;

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('kb.lifecycle.context_stale_enabled', 'true', 'kb',
     'M14.3: marca context-stale le note active i cui file coperti vengono modificati da un run successivo non collegato alla nota (segnalazione, non cancellazione).', FALSE)
ON CONFLICT (key) DO NOTHING;
