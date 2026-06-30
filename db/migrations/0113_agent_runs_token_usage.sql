-- Migrazione: aggiunge colonne token/costo alla tabella agent_runs
-- per rendere visibile il consumo token nel frontend chat.
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS prompt_tokens    INT NOT NULL DEFAULT 0;
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS completion_tokens INT NOT NULL DEFAULT 0;
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS total_tokens     INT NOT NULL DEFAULT 0;
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS total_cost       DOUBLE PRECISION NOT NULL DEFAULT 0.0;
