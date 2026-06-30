-- Aggiunge campo analisi progetto
ALTER TABLE projects ADD COLUMN IF NOT EXISTS analysis_json JSONB;
ALTER TABLE projects ADD COLUMN IF NOT EXISTS analyzed_at TIMESTAMPTZ;
