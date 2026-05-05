-- Terminal commands: aggiunge campi per completamento evento-driven
ALTER TABLE terminal_commands
  ADD COLUMN IF NOT EXISTS exit_code INTEGER,
  ADD COLUMN IF NOT EXISTS finished_at TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS full_output TEXT;
