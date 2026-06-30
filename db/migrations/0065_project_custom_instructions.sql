-- Aggiunge custom_instructions al progetto: istruzioni specifiche per-progetto
-- iniettate nel system prompt di ogni run dell'agente.
--
-- Popolato automaticamente da analyze_project() in base allo stack rilevato
-- (es. "esegui pnpm verify prima di concludere" per progetti Next.js/TypeScript).
-- Modificabile manualmente dall'utente tramite API PATCH /api/projects/:id.
--
-- Separato da analysis_json (output read-only dell'analisi) perché
-- custom_instructions è input editabile che l'utente può sovrascrivere.

ALTER TABLE projects
  ADD COLUMN IF NOT EXISTS custom_instructions TEXT;

COMMENT ON COLUMN projects.custom_instructions IS
  'Istruzioni specifiche per-progetto iniettate nel system prompt di ogni run agente. '
  'Auto-generate da analyze_project() e modificabili manualmente.';
