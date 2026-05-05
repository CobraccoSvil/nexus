-- Migrazione 0087: Metadati per la pagina admin prompt v2
--
-- Aggiunge tre colonne a nexus_prompt_templates per supportare:
--   - schema_type: 'plain' (legacy) | 'xml' (v2 strutturato)
--   - placeholder_vars: lista dei placeholder che il prompt richiede
--   - experimental: flag per varianti generate dal PromptOptimizerWorker
--                   (Fase 3); ora false per tutti, useremo in canary A/B.
--
-- E' additiva: nessuna riga viene cancellata, nessuna colonna esistente toccata.

ALTER TABLE nexus_prompt_templates
  ADD COLUMN IF NOT EXISTS schema_type TEXT NOT NULL DEFAULT 'plain'
    CHECK (schema_type IN ('plain', 'xml'));

ALTER TABLE nexus_prompt_templates
  ADD COLUMN IF NOT EXISTS placeholder_vars JSONB NOT NULL DEFAULT '[]'::jsonb;

ALTER TABLE nexus_prompt_templates
  ADD COLUMN IF NOT EXISTS experimental BOOLEAN NOT NULL DEFAULT FALSE;

CREATE INDEX IF NOT EXISTS idx_nexus_prompt_templates_schema_experimental
  ON nexus_prompt_templates (schema_type, experimental);

-- Marca i 4 prompt v2 della migrazione 0086 come schema_type='xml' e
-- popola la lista dei placeholder che usano (utile alla UI per generare
-- l'anteprima resa con valori d'esempio).

UPDATE nexus_prompt_templates
SET schema_type = 'xml',
    placeholder_vars = '["lang_hint","type_hint","repo_summary"]'::jsonb,
    updated_at = NOW(),
    updated_by = 'migration_0087'
WHERE key IN (
  'agent.coder.base',
  'agent.general.debugger',
  'agent.tester.base',
  'agent.reviewer.general'
);
