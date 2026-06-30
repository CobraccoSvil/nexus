-- Cache dei suggerimenti run-config rilevati automaticamente dal filesystem.
-- Popolato da detect_run_configs (on-demand) e da analyze_project (analisi progetto).
-- detect_run_configs legge da qui se il dato è fresco (< 7 giorni), altrimenti riscansiona.
ALTER TABLE projects
    ADD COLUMN IF NOT EXISTS detected_suggestions      JSONB,
    ADD COLUMN IF NOT EXISTS detected_suggestions_at   TIMESTAMPTZ;
