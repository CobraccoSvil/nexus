-- Migration 0039: aggiunge supervisor_mode ad agent_runs per preservarlo durante il resume
ALTER TABLE agent_runs
    ADD COLUMN IF NOT EXISTS supervisor_mode TEXT NOT NULL DEFAULT 'none';

COMMENT ON COLUMN agent_runs.supervisor_mode IS 'Modalità supervisore (none/anomaly/interleaved/continuous) — ereditata dal run originale durante resume';
