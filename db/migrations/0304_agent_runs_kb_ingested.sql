-- ADR 0017 v2 TODO 7 — flag idempotenza per il worker run-summary.
--
-- Il worker `wiki::run_summary_worker` scansiona periodicamente `agent_runs`
-- in stato terminale (status IN ('completed','failed','aborted')) e crea un
-- wiki_doc (scope=project, kind='run_summary') con il riepilogo del run.

BEGIN;

ALTER TABLE agent_runs
    ADD COLUMN IF NOT EXISTS kb_ingested BOOLEAN;

CREATE INDEX IF NOT EXISTS idx_agent_runs_kb_ingest_pending
    ON agent_runs (completed_at DESC)
    WHERE kb_ingested IS NULL
      AND status IN ('completed', 'failed', 'aborted');

COMMIT;
