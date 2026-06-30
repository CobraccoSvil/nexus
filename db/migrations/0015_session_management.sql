-- Discriminatore per riassunti di sessione vs correzioni prompt standard
ALTER TABLE prompt_corrections
  ADD COLUMN IF NOT EXISTS type TEXT NOT NULL DEFAULT 'correction';

-- message_id diventa nullable per i record di tipo session_memory
ALTER TABLE prompt_corrections
  ALTER COLUMN message_id DROP NOT NULL;

CREATE INDEX IF NOT EXISTS idx_prompt_corrections_type
  ON prompt_corrections(project_id, type);
