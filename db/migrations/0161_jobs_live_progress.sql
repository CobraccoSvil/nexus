-- 0161: aggiunge campi per il monitoraggio live di run Playwright (e simili)
-- nella tabella jobs.
--
-- Motivo: prima il tool run_playwright_tests faceva una sola INSERT a fine
-- esecuzione, quindi il pannello Playwright vedeva solo lo storico post-run.
-- Per il polling live (counter passed/failed, spec corrente) servono campi
-- aggiornabili incrementalmente.

ALTER TABLE jobs
  ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  ADD COLUMN IF NOT EXISTS output_log TEXT,
  ADD COLUMN IF NOT EXISTS progress JSONB NOT NULL DEFAULT '{}'::jsonb;

-- Indice per polling efficiente del frontend: il pannello chiede "dammi i jobs
-- recenti per progetto", e quando filtra per status='running' deve essere veloce.
CREATE INDEX IF NOT EXISTS idx_jobs_project_status_updated
  ON jobs (project_id, status, updated_at DESC);

-- Trigger per aggiornare updated_at automaticamente su ogni UPDATE.
-- Idempotente: la funzione potrebbe gia' esistere da altre tabelle.
CREATE OR REPLACE FUNCTION jobs_set_updated_at()
RETURNS TRIGGER AS $$
BEGIN
  NEW.updated_at = NOW();
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_jobs_updated_at ON jobs;
CREATE TRIGGER trg_jobs_updated_at
  BEFORE UPDATE ON jobs
  FOR EACH ROW
  EXECUTE FUNCTION jobs_set_updated_at();
