-- 0007_kb_ingest_canonical_outcomes.sql
-- Allinea l'indice parziale del worker di ingest KB (run_summary_worker.rs) agli
-- esiti TERMINALI canonici della macchina a stati (mig 0386 + completed_unverified
-- di mig 0534): il filtro precedente ('completed','failed','aborted') dimenticava
-- gli esiti canonici, quindi un run verificato (completed_verified), non verificato
-- (completed_unverified) o diagnosticato (failed_diagnosed) NON entrava nella
-- memoria episodica del progetto (wiki_doc kind='run_summary').
--
-- L'indice PARZIALE deve elencare gli stessi status della query del worker,
-- altrimenti Postgres non lo usa per le nuove righe (full scan). DROP + CREATE
-- della sola condizione WHERE; la colonna indicizzata (completed_at DESC) e la
-- semantica (kb_ingested IS NULL) restano invariate.

DROP INDEX IF EXISTS idx_agent_runs_kb_ingest_pending;

CREATE INDEX IF NOT EXISTS idx_agent_runs_kb_ingest_pending
  ON public.agent_runs USING btree (completed_at DESC)
  WHERE (
    (kb_ingested IS NULL)
    AND (status = ANY (ARRAY[
      'completed'::text,
      'completed_verified'::text,
      'completed_unverified'::text,
      'failed'::text,
      'failed_diagnosed'::text,
      'aborted'::text
    ]))
  );
