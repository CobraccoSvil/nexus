-- Migrazione 0103: estende il check constraint di nexus_project_insights
-- per includere il valore 'running'.
--
-- Motivazione: il refactor di deep-analyze asincrono (mig 0102 + Rust handler
-- crates/mcp-core/src/projects/deep_analyze.rs) inserisce una riga con
-- status='running' immediatamente all'avvio del job, e poi la aggiorna a
-- 'completed' / 'failed' quando il task background termina.
-- Il check constraint originale (mig 0093) accettava solo
-- ['completed', 'partial', 'failed'] e bloccava l'INSERT.
--
-- Bug osservato: "new row for relation nexus_project_insights violates check
-- constraint nexus_project_insights_status_check" (HTTP 500 dal POST
-- /api/projects/:id/deep-analyze).

ALTER TABLE nexus_project_insights
    DROP CONSTRAINT IF EXISTS nexus_project_insights_status_check;

ALTER TABLE nexus_project_insights
    ADD CONSTRAINT nexus_project_insights_status_check
    CHECK (status IN ('running', 'completed', 'partial', 'failed'));
