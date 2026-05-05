-- Migration 0034: aggiunge colonna messages_json ad agent_runs per supportare ripresa dopo interruzione

ALTER TABLE agent_runs
    ADD COLUMN IF NOT EXISTS messages_json TEXT;

COMMENT ON COLUMN agent_runs.messages_json IS 'Serializzazione JSON della history LLM (messages[]) al momento dell''ultima iterazione — usata per riprendere run interrotti';
