-- Migrazione: supporto agenti paralleli con gerarchia padre/figlio
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS parent_run_id UUID REFERENCES agent_runs(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS idx_agent_runs_parent ON agent_runs(parent_run_id);
